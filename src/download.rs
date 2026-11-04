//! Pobieranie kafli na żądanie: pobrany kafel jest od razu wypiekany i zapisywany na dysku.

use std::collections::{HashMap, HashSet, VecDeque};
use std::io::Read;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::thread;
use std::time::Duration;

use crate::bake;
use crate::geo::TileId;
use crate::store::{Layer, Store};

const USER_AGENT: &str = concat!(
    "PolskieStudioMap/",
    env!("CARGO_PKG_VERSION"),
    " (+https://redzikowski.com/portfolio/polskie-studio-map)"
);

/// Najwyższy poziom kafla terenu 256 px (około 0,8 m na piksel w Polsce): NMT GUGiK z WCS.
pub const TERRAIN_MAX_Z: u8 = 17;
/// Najwyższy poziom z Mapterhorn: ćwiartka kafla 512 px poziomu 15 źródła.
const MAPTERHORN_MAX_Z: u8 = 16;
/// Od tego poziomu w trybie „model pokrycia” teren pochodzi z NMPT (każdy kafel to kilka sekund pracy usługi).
const SURFACE_MIN_Z: u8 = 16;

static SURFACE: AtomicBool = AtomicBool::new(false);

/// Tryb „drzewa i budynki z lidaru”.
pub fn set_surface(on: bool) {
    SURFACE.store(on, Ordering::Relaxed);
}

/// Warstwa na dysku, z której pochodzi kafel terenu.
pub fn dem_layer(t: TileId) -> Layer {
    if SURFACE.load(Ordering::Relaxed) && t.z >= SURFACE_MIN_Z {
        Layer::Dsm
    } else {
        Layer::Dem
    }
}

/// Kafel terenu idzie przez wolną usługę WCS.
pub fn dem_from_wcs(t: TileId) -> bool {
    t.z == TERRAIN_MAX_Z || dem_layer(t) == Layer::Dsm
}

/// Kafel pobierany właśnie przez ten wątek. Inny wątek chcący ten sam kafel czeka i bierze go z dysku.
struct Claim(Layer, TileId);

fn claims() -> &'static (Mutex<HashSet<(Layer, TileId)>>, Condvar) {
    static C: OnceLock<(Mutex<HashSet<(Layer, TileId)>>, Condvar)> = OnceLock::new();
    C.get_or_init(Default::default)
}

impl Claim {
    fn take(layer: Layer, t: TileId) -> Claim {
        let (set, cv) = claims();
        let mut s = set.lock().unwrap();
        while !s.insert((layer, t)) {
            s = cv.wait(s).unwrap();
        }
        Claim(layer, t)
    }
}

impl Drop for Claim {
    fn drop(&mut self) {
        let (set, cv) = claims();
        set.lock().unwrap().remove(&(self.0, self.1));
        cv.notify_all();
    }
}

/// Zdekodowane zdjęcia satelitarne: jeden przodek służy wielu kaflom ortofoto.
const SAT_CACHE: usize = 256;

fn decoded_sat(store: &Store, t: TileId) -> Option<Arc<image::RgbaImage>> {
    type Cache = (HashMap<TileId, Arc<image::RgbaImage>>, VecDeque<TileId>);
    static CACHE: OnceLock<Mutex<Cache>> = OnceLock::new();
    let cache = CACHE.get_or_init(Default::default);
    if let Some(img) = cache.lock().unwrap().0.get(&t) {
        return Some(img.clone());
    }
    let blob = get(store, Layer::Sat, t).ok()?;
    let img = image::load_from_memory_with_format(bake::jpeg(&blob)?, image::ImageFormat::Jpeg).ok()?.to_rgba8();
    let img = Arc::new(img);
    let mut c = cache.lock().unwrap();
    if c.0.insert(t, img.clone()).is_none() {
        c.1.push_back(t);
    }
    if c.1.len() > SAT_CACHE {
        if let Some(old) = c.1.pop_front() {
            c.0.remove(&old);
        }
    }
    Some(img)
}

pub fn agent() -> &'static ureq::Agent {
    static AGENT: OnceLock<ureq::Agent> = OnceLock::new();
    AGENT.get_or_init(|| {
        ureq::AgentBuilder::new()
            .user_agent(USER_AGENT)
            .timeout_connect(Duration::from_secs(10))
            .timeout_read(Duration::from_secs(30))
            .max_idle_connections_per_host(32)
            .build()
    })
}

#[derive(Debug)]
pub enum FetchError {
    /// Źródło nie ma tego kafla (np. morze poza zasięgiem zdjęć) – nie ponawiamy.
    Missing,
    Network(String),
}

impl std::fmt::Display for FetchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FetchError::Missing => write!(f, "brak kafla w źródle"),
            FetchError::Network(e) => write!(f, "{e}"),
        }
    }
}

/// Pobranie kafla z sieci (3 próby z narastającą przerwą).
pub fn fetch(layer: Layer, t: TileId) -> Result<Vec<u8>, FetchError> {
    fetch_url(&layer.url(t))
}

fn fetch_url(url: &str) -> Result<Vec<u8>, FetchError> {
    let mut last = String::new();
    for attempt in 0..3 {
        if attempt > 0 {
            thread::sleep(Duration::from_millis(400 * (1 << attempt)));
        }
        match agent().get(url).call() {
            Ok(r) => {
                let mut buf = Vec::with_capacity(64 * 1024);
                match r.into_reader().take(16 << 20).read_to_end(&mut buf) {
                    Ok(_) if !buf.is_empty() => return Ok(buf),
                    Ok(_) => last = "pusta odpowiedź".into(),
                    Err(e) => last = e.to_string(),
                }
            }
            Err(ureq::Error::Status(404 | 400, _)) => return Err(FetchError::Missing),
            Err(ureq::Error::Status(code, _)) => last = format!("serwer odpowiedział kodem {code}"),
            Err(ureq::Error::Transport(e)) => last = e.to_string(),
        }
    }
    Err(FetchError::Network(last))
}

/// Źródło kafla: zwykłe (jeden adres) albo składane z usług krajowych.
fn fetch_any(store: &Store, layer: Layer, t: TileId) -> Result<Vec<u8>, FetchError> {
    match layer {
        Layer::Ortho => crate::national::fetch_ortho(t, sat_background(store, t)),
        _ => fetch(layer, t),
    }
}

/// Tło ortofoto: zdjęcie satelitarne (przodek do z14), przycięte do kafla.
pub fn sat_background(store: &Store, t: TileId) -> Option<image::RgbaImage> {
    (0..=t.z.saturating_sub(1)).find_map(|dz| {
        let (a, off, k) = if t.z - dz > crate::render::tiles::SAT_MAX_Z {
            return None;
        } else if dz == 0 {
            (t, [0.0, 0.0], 1.0)
        } else {
            t.ancestor(dz)
        };
        let img = decoded_sat(store, a)?;
        let s = img.width() as f32;
        let (x, y, w) = ((off[0] * s) as u32, (off[1] * s) as u32, ((k * s) as u32).max(1));
        Some(image::imageops::crop_imm(&*img, x, y, w, w).to_image())
    })
}

/// Kafel z dysku: dane, wiadomy brak w źródle albo `None`, gdy trzeba pobrać.
fn stored(store: &Store, layer: Layer, t: TileId) -> Option<Result<Vec<u8>, FetchError>> {
    match store.read(layer, t) {
        Some(b) if bake::dem_stale(&b) => None,
        Some(b) => Some(Ok(b)),
        None if store.has(layer, t) => Some(Err(FetchError::Missing)),
        None => None,
    }
}

/// Teren z pliku Terrarium. Źródło miewa miejsca bez danych – uzupełnia je kafel nadrzędny.
fn bake_terrain(store: &Store, t: TileId, mut elev: Vec<f32>, w: usize, h: usize) -> Result<Vec<u8>, FetchError> {
    if bake::has_holes(&elev) {
        let parent = (t.z > 0).then(|| t.ancestor(1)).and_then(|(p, off, k)| {
            let d = get(store, Layer::Dem, p).ok().and_then(|b| bake::decode_dem(&b))?;
            Some((d, off, k))
        });
        let view = parent.as_ref().map(|(d, off, k)| (d, *off, *k));
        if !bake::fill_holes(&mut elev, w, h, view) {
            return Err(FetchError::Network("kafel terenu bez danych".into()));
        }
    }
    Ok(bake::encode_dem(&elev, w, h))
}

/// Kafel terenu z Mapterhorn: jeden plik 512 px poziomu wyżej to cztery kafle 256 px, więc od razu
/// zapisują się wszystkie. Zwraca kafel `t`.
fn fetch_mapterhorn(store: &Store, t: TileId) -> Result<Vec<u8>, FetchError> {
    let p = t.ancestor(1).0;
    let src = fetch_url(&format!("https://tiles.mapterhorn.com/{}/{}/{}.webp", p.z, p.x, p.y))?;
    let net = |e: bake::BakeError| FetchError::Network(e.to_string());
    let (elev, w, h) = bake::terrarium(&src).map_err(net)?;
    if (w, h) != (512, 512) {
        return Err(FetchError::Network("nieoczekiwany rozmiar kafla terenu".into()));
    }
    let own = bake_terrain(store, t, bake::quadrant(&elev, (t.x % 2) as usize, (t.y % 2) as usize), 256, 256)?;
    for (qx, qy) in [(0u32, 0u32), (1, 0), (0, 1), (1, 1)] {
        let c = TileId::new(t.z, p.x * 2 + qx, p.y * 2 + qy);
        if c == t || matches!(stored(store, Layer::Dem, c), Some(Ok(_))) {
            continue;
        }
        let part = bake::quadrant(&elev, qx as usize, qy as usize);
        // rodzeństwo z brakami pobierze się samo, gdy będzie potrzebne
        if !bake::has_holes(&part) {
            let _ = store.write(Layer::Dem, c, Some(&bake::encode_dem(&part, 256, 256)));
        }
    }
    Ok(own)
}

/// Siatka ASCII z WCS (odpowiedź wieloczęściowa): wysokości od górnego wiersza, braki poniżej progu dziur.
fn parse_aaigrid(body: &str, n: usize) -> Result<Vec<f32>, String> {
    let at = body.find("ncols").ok_or("usługa nie zwróciła siatki")?;
    let mut tokens = body[at..].split_ascii_whitespace();
    let mut nodata = None;
    let mut first = None;
    while let Some(k) = tokens.next() {
        if k.parse::<f32>().is_ok() {
            first = Some(k);
            break;
        }
        let v = tokens.next().ok_or("urwany nagłówek siatki")?;
        if k.eq_ignore_ascii_case("nodata_value") {
            nodata = v.parse::<f32>().ok();
        }
    }
    let values: Vec<f32> = first
        .into_iter()
        .chain(tokens)
        .take(n * n)
        .map(|t| t.parse::<f32>().map_err(|_| "błędna wartość siatki".to_string()))
        .collect::<Result<_, _>>()?;
    if values.len() != n * n {
        return Err("niepełna siatka".into());
    }
    Ok(values.into_iter().map(|v| if Some(v) == nodata || v < -500.0 || !v.is_finite() { -32768.0 } else { v }).collect())
}

/// Kafel z usługi WCS GUGiK (NMT albo NMPT, rozdzielczość 1 m).
fn fetch_wcs(store: &Store, t: TileId, surface: bool) -> Result<Vec<u8>, FetchError> {
    const CRS: &str = "http://www.opengis.net/def/crs/EPSG/0/3857";
    let (service, coverage) = if surface {
        ("NMPT/GRID1/WCS/DigitalSurfaceModel", "DSM_PL-EVRF2007-NH")
    } else {
        ("NMT/GRID1/WCS/DigitalTerrainModel", "DTM_PL-EVRF2007-NH")
    };
    let (x0, y0, x1, y1) = crate::national::merc_bounds(t);
    let url = format!(
        "https://mapy.geoportal.gov.pl/wss/service/PZGIK/{service}?SERVICE=WCS&REQUEST=GetCoverage&VERSION=2.0.1&COVERAGEID={coverage}&FORMAT=image/x-aaigrid&SUBSETTINGCRS={CRS}&OUTPUTCRS={CRS}&SUBSET=x({x0:.2},{x1:.2})&SUBSET=y({y0:.2},{y1:.2})&SCALESIZE=x(256),y(256)"
    );
    let b = crate::national::get_bytes(&url, 8 << 20)?;
    let elev = parse_aaigrid(&String::from_utf8_lossy(&b), 256).map_err(FetchError::Network)?;
    bake_terrain(store, t, elev, 256, 256)
}

/// Teren: na najbliższych kaflach NMT GUGiK, dalej Mapterhorn, a gdy niedostępny – AWS (do poziomu 15).
fn fetch_terrain(store: &Store, t: TileId) -> Result<Vec<u8>, FetchError> {
    if t.z > MAPTERHORN_MAX_Z {
        return fetch_wcs(store, t, false);
    }
    if (1..=MAPTERHORN_MAX_Z).contains(&t.z) {
        match fetch_mapterhorn(store, t) {
            Ok(b) => return Ok(b),
            Err(FetchError::Missing) => {}
            Err(FetchError::Network(e)) => log::warn!("teren {}/{}/{}: {e}", t.z, t.x, t.y),
        }
    }
    if t.z > 15 {
        return Err(FetchError::Missing);
    }
    let src = fetch(Layer::Dem, t)?;
    let (elev, w, h) = bake::terrarium(&src).map_err(|e| FetchError::Network(e.to_string()))?;
    bake_terrain(store, t, elev, w, h)
}

/// Wypieczony kafel z dysku albo z sieci (pobrany jest od razu wypiekany i zapisywany).
pub fn get(store: &Store, layer: Layer, t: TileId) -> Result<Vec<u8>, FetchError> {
    if !crate::national::tile_in_poland(t) {
        return Err(FetchError::Missing);
    }
    let layer = if layer == Layer::Dem { dem_layer(t) } else { layer };
    if let Some(r) = stored(store, layer, t) {
        return r;
    }
    let _claim = Claim::take(layer, t);
    if let Some(r) = stored(store, layer, t) {
        return r;
    }
    let fetched = if layer == Layer::Dsm {
        fetch_wcs(store, t, true)
    } else if layer == Layer::Dem {
        fetch_terrain(store, t)
    } else {
        fetch_any(store, layer, t).and_then(|src| bake::bake(layer, &src).map_err(|e| FetchError::Network(e.to_string())))
    };
    match fetched {
        Ok(baked) => {
            let _ = store.write(layer, t, Some(&baked));
            Ok(baked)
        }
        Err(FetchError::Missing) => {
            let _ = store.write(layer, t, None);
            Err(FetchError::Missing)
        }
        Err(e) => Err(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_multipart_grid() {
        let body = "--wcs\nContent-Type: image/x-aaigrid\nContent-Disposition: INLINE; filename=result.asc\n\nncols 2\nnrows 2\nxllcorner 1\nyllcorner 1\ncellsize 1\nNODATA_value -9999\n 200.5 201\n -9999 203\n--wcs--\n";
        let v = parse_aaigrid(body, 2).unwrap();
        assert_eq!(v, vec![200.5, 201.0, -32768.0, 203.0]);
        assert!(parse_aaigrid("<xml>błąd</xml>", 2).is_err());
        assert!(parse_aaigrid("ncols 2\nnrows 2\n 1 2", 2).is_err());
    }
}
