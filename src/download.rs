//! Pobieranie kafli na żądanie: pobrany kafel jest od razu wypiekany i zapisywany na dysku.

use std::collections::{HashMap, HashSet, VecDeque};
use std::io::Read;
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

/// Najwyższy poziom kafla terenu 256 px: ćwiartka kafla 512 px poziomu 15 źródła.
pub const TERRAIN_MAX_Z: u8 = 16;

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

/// Teren: najpierw Mapterhorn (krajowe modele LiDAR), a gdy niedostępny – AWS (do poziomu 15).
fn fetch_terrain(store: &Store, t: TileId) -> Result<Vec<u8>, FetchError> {
    if (1..=TERRAIN_MAX_Z).contains(&t.z) {
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
    if let Some(r) = stored(store, layer, t) {
        return r;
    }
    let _claim = Claim::take(layer, t);
    if let Some(r) = stored(store, layer, t) {
        return r;
    }
    let fetched = if layer == Layer::Dem {
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
