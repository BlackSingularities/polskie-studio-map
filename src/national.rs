//! Zasięg programu (Polska, granica z Natural Earth 1:50m) i ortofotomapa GUGiK.

use std::io::Read;
use std::sync::{Condvar, Mutex, OnceLock};
use std::time::Duration;

use serde::Deserialize;

use crate::download::{agent, FetchError};
use crate::geo::{x_to_lon, y_to_lat, TileId};

#[derive(Deserialize)]
struct Country {
    ring: Vec<[f64; 2]>,
}

#[derive(Deserialize)]
struct Coverage {
    countries: Vec<Country>,
}

fn coverage() -> &'static [Country] {
    static C: OnceLock<Vec<Country>> = OnceLock::new();
    C.get_or_init(|| {
        serde_json::from_str::<Coverage>(include_str!("../assets/coverage.json"))
            .map(|c| c.countries)
            .unwrap_or_default()
    })
}

pub fn in_ring(lon: f64, lat: f64, ring: &[[f64; 2]]) -> bool {
    let mut inside = false;
    let mut j = ring.len().wrapping_sub(1);
    for i in 0..ring.len() {
        let ([xi, yi], [xj, yj]) = (ring[i], ring[j]);
        if (yi > lat) != (yj > lat) && lon < (xj - xi) * (lat - yi) / (yj - yi) + xi {
            inside = !inside;
        }
        j = i;
    }
    inside
}

/// Czy kafel (z marginesem `pad` ułamka kafla) dotyka Polski.
fn tile_touches(t: TileId, pad: f64) -> bool {
    let n = (1u64 << t.z) as f64;
    for j in 0..=4 {
        for i in 0..=4 {
            let fx = (t.x as f64 - pad + (1.0 + 2.0 * pad) * i as f64 / 4.0) / n;
            let fy = (t.y as f64 - pad + (1.0 + 2.0 * pad) * j as f64 / 4.0) / n;
            if in_poland(x_to_lon(fx), y_to_lat(fy)) {
                return true;
            }
        }
    }
    false
}

/// Prostokąt obejmujący Polskę: (zachód, południe, wschód, północ).
const BOUNDS: (f64, f64, f64, f64) = (14.07, 49.0, 24.15, 54.84);
/// Zapas wokół granicy (~10 km).
const MARGIN: f64 = 0.15;

/// Czy punkt leży w Polsce.
pub fn in_poland(lon: f64, lat: f64) -> bool {
    coverage().iter().any(|c| in_ring(lon, lat, &c.ring))
}

/// Położenie kamery ograniczone do Polski z zapasem.
pub fn clamp_to_poland(lon: f64, lat: f64) -> (f64, f64) {
    (
        lon.clamp(BOUNDS.0 - MARGIN, BOUNDS.2 + MARGIN),
        lat.clamp(BOUNDS.1 - MARGIN, BOUNDS.3 + MARGIN),
    )
}

/// Czy kafel dotyka Polski (z zapasem). Kafli spoza Polski nie pobieramy.
pub fn tile_in_poland(t: TileId) -> bool {
    let b = t.bbox();
    let (w, s, e, n) = (b.w - MARGIN, b.s - MARGIN, b.e + MARGIN, b.n + MARGIN);
    if e < BOUNDS.0 || w > BOUNDS.2 || n < BOUNDS.1 || s > BOUNDS.3 {
        return false;
    }
    // punkt granicy w kaflu (kafel większy od kraju) albo próbka kafla w kraju (kafel w środku kraju)
    let border = coverage()
        .iter()
        .flat_map(|c| c.ring.iter())
        .any(|p| p[0] >= w && p[0] <= e && p[1] >= s && p[1] <= n);
    border
        || (0..=4).any(|j| {
            (0..=4).any(|i| in_poland(w + (e - w) * i as f64 / 4.0, s + (n - s) * j as f64 / 4.0))
        })
}

/// Najniższy poziom z ortofoto (niżej wystarcza Sentinel-2).
pub const ORTHO_MIN_Z: u8 = 13;
pub const ORTHO_MAX_Z: u8 = 17;
/// Rozmiar obrazu ortofoto na kafel (px).
pub const ORTHO_PX: u32 = 512;

/// Czy kafel (dowolnego poziomu) ma ortofoto.
pub fn has_ortho(t: TileId) -> bool {
    tile_touches(t, 0.0)
}

const WM: f64 = 20_037_508.342_789_244;

/// Granice kafla w metrach EPSG:3857: (x0, y0, x1, y1).
pub fn merc_bounds(t: TileId) -> (f64, f64, f64, f64) {
    let n = (1u64 << t.z) as f64;
    let x0 = t.x as f64 / n * 2.0 * WM - WM;
    let x1 = (t.x + 1) as f64 / n * 2.0 * WM - WM;
    let y1 = WM - t.y as f64 / n * 2.0 * WM;
    let y0 = WM - (t.y + 1) as f64 / n * 2.0 * WM;
    (x0, y0, x1, y1)
}

/// Usługa krajowa jest wolna: najwyżej tyle zapytań naraz, żeby nie zająć wszystkich wątków kafli.
const MAX_NATIONAL: usize = 6;

struct Permit;

impl Permit {
    fn gate() -> &'static (Mutex<usize>, Condvar) {
        static G: OnceLock<(Mutex<usize>, Condvar)> = OnceLock::new();
        G.get_or_init(Default::default)
    }

    fn take() -> Permit {
        let (m, cv) = Self::gate();
        let mut n = m.lock().unwrap();
        while *n >= MAX_NATIONAL {
            n = cv.wait(n).unwrap();
        }
        *n += 1;
        Permit
    }
}

impl Drop for Permit {
    fn drop(&mut self) {
        let (m, cv) = Self::gate();
        *m.lock().unwrap() -= 1;
        cv.notify_one();
    }
}

pub fn get_bytes(url: &str, limit: u64) -> Result<Vec<u8>, FetchError> {
    let _permit = Permit::take();
    let mut last = String::new();
    for attempt in 0..3 {
        if attempt > 0 {
            std::thread::sleep(Duration::from_millis(600 * (1 << attempt)));
        }
        match agent().get(url).timeout(Duration::from_secs(90)).call() {
            Ok(r) => {
                let mut buf = Vec::new();
                match r.into_reader().take(limit).read_to_end(&mut buf) {
                    Ok(_) if !buf.is_empty() => return Ok(buf),
                    Ok(_) => last = "pusta odpowiedź".into(),
                    Err(e) => last = e.to_string(),
                }
            }
            Err(ureq::Error::Status(404 | 400, _)) => return Err(FetchError::Missing),
            Err(ureq::Error::Status(c, _)) => last = format!("serwer odpowiedział kodem {c}"),
            Err(ureq::Error::Transport(e)) => last = e.to_string(),
        }
    }
    Err(FetchError::Network(last))
}

/// Ortofoto kafla (przycięte do Polski) na tle zdjęcia satelitarnego.
/// Zwraca JPEG albo `Missing`, gdy usługa nie ma obrazu.
pub fn fetch_ortho(t: TileId, background: Option<image::RgbaImage>) -> Result<Vec<u8>, FetchError> {
    if t.z < ORTHO_MIN_Z || !tile_touches(t, 0.0) {
        return Err(FetchError::Missing);
    }
    let (x0, y0, x1, y1) = merc_bounds(t);
    let bb = format!("{x0:.2},{y0:.2},{x1:.2},{y1:.2}");
    let n = ORTHO_PX;
    let mut canvas = match background {
        Some(b) => image::imageops::resize(&b, n, n, image::imageops::FilterType::Triangle),
        None => image::RgbaImage::from_pixel(n, n, image::Rgba([96, 104, 88, 255])),
    };
    let mut any = false;
    let mut net_err = None;
    let nz = (1u64 << t.z) as f64;
    // obraz przycinamy do granicy Polski (siatka co 8 px)
    const CELL: u32 = 8;
    let cells = n / CELL;
    let mask: Vec<bool> = (0..cells * cells)
        .map(|i| {
            let (cx, cy) = ((i % cells) as f64 + 0.5, (i / cells) as f64 + 0.5);
            in_poland(x_to_lon((t.x as f64 + cx / cells as f64) / nz), y_to_lat((t.y as f64 + cy / cells as f64) / nz))
        })
        .collect();
    let url = format!("https://mapy.geoportal.gov.pl/wss/service/PZGIK/ORTO/WMS/HighResolution?SERVICE=WMS&REQUEST=GetMap&VERSION=1.3.0&LAYERS=Raster&STYLES=&CRS=EPSG:3857&BBOX={bb}&WIDTH={n}&HEIGHT={n}&FORMAT=image/png&TRANSPARENT=TRUE");
    match get_bytes(&url, 16 << 20) {
        Ok(b) => {
            if let Some(img) = image::load_from_memory(&b).ok().map(|i| i.to_rgba8()).filter(|i| i.width() == n && i.height() == n) {
                let mut opaque = 0usize;
                for (i, (dst, s)) in canvas.pixels_mut().zip(img.pixels()).enumerate() {
                    let (px, py) = (i as u32 % n, i as u32 / n);
                    if !mask[((py / CELL) * cells + px / CELL) as usize] {
                        continue;
                    }
                    let a = s[3] as u32;
                    if a == 0 {
                        continue;
                    }
                    // usługa czasem zwraca białe tło zamiast przezroczystości poza zasięgiem
                    if s[0] > 250 && s[1] > 250 && s[2] > 250 {
                        continue;
                    }
                    opaque += 1;
                    for c in 0..3 {
                        dst[c] = ((s[c] as u32 * a + dst[c] as u32 * (255 - a)) / 255) as u8;
                    }
                }
                any = opaque > (n * n / 50) as usize;
            }
        }
        Err(FetchError::Network(e)) => net_err = Some(e),
        Err(FetchError::Missing) => {}
    }
    // kafla bez odpowiedzi usługi nie zapisujemy – spróbujemy później
    if let Some(e) = net_err {
        return Err(FetchError::Network(e));
    }
    if !any {
        return Err(FetchError::Missing);
    }
    let rgb = image::DynamicImage::ImageRgba8(canvas).to_rgb8();
    let mut out = Vec::new();
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, 86)
        .encode_image(&rgb)
        .map_err(|e| FetchError::Network(e.to_string()))?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn coverage_is_poland_only() {
        assert!(in_poland(19.94, 50.06)); // Kraków
        assert!(!in_poland(14.42, 50.08)); // Praga
        assert!(!in_poland(17.11, 48.15)); // Bratysława
        assert!(tile_in_poland(crate::geo::tile_at(19.94, 50.06, 14)));
        assert!(tile_in_poland(TileId::new(0, 0, 0)));
        assert!(!tile_in_poland(crate::geo::tile_at(2.35, 48.85, 10))); // Paryż
        assert!(!tile_in_poland(crate::geo::tile_at(14.42, 50.08, 12)));
        assert_eq!(clamp_to_poland(2.0, 60.0), (14.07 - MARGIN, 54.84 + MARGIN));
    }
}
