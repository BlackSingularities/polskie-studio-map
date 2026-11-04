//! Warstwy tematyczne: katalog usług (`assets/layers.json`), kafle WMS w Web Mercatorze z pamięcią
//! na dysku (`overlays\<warstwa>\z\x_y.png`), legendy i zestawy warstw.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use serde::{Deserialize, Serialize};

use crate::download::FetchError;
use crate::geo::{y_to_lat, TileId};
use crate::national::{self, get_bytes, merc_bounds};

/// Rozmiar kafla warstwy (px).
pub const TILE_PX: u32 = 256;
/// Najwyższy poziom kafli warstw (dalej powiększenie).
pub const MAX_Z: u8 = 18;

#[derive(Deserialize)]
struct Raw {
    services: Vec<RawService>,
    presets: Vec<Preset>,
}

#[derive(Deserialize)]
struct RawService {
    group: String,
    url: String,
    #[serde(default = "merc")]
    crs: u32,
    attribution: String,
    min_z: u8,
    layers: Vec<RawLayer>,
}

#[derive(Deserialize)]
struct RawLayer {
    id: String,
    name: String,
    layer: String,
    #[serde(default)]
    desc: String,
    min_z: Option<u8>,
    #[serde(default = "full")]
    opacity: f32,
}

fn merc() -> u32 {
    3857
}

fn full() -> f32 {
    0.85
}

/// Warstwa z katalogu.
pub struct LayerDef {
    pub id: String,
    pub name: String,
    pub group: String,
    pub desc: String,
    pub attribution: String,
    pub min_z: u8,
    pub opacity: f32,
    url: String,
    crs: u32,
    layer: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Preset {
    pub name: String,
    pub layers: Vec<String>,
}

pub struct Catalog {
    pub layers: Vec<LayerDef>,
    pub presets: Vec<Preset>,
}

pub fn catalog() -> &'static Catalog {
    static C: OnceLock<Catalog> = OnceLock::new();
    C.get_or_init(|| {
        let raw: Raw = serde_json::from_str(include_str!("../assets/layers.json")).expect("katalog warstw");
        let mut layers = Vec::new();
        for s in raw.services {
            for l in s.layers {
                layers.push(LayerDef {
                    id: l.id,
                    name: l.name,
                    group: s.group.clone(),
                    desc: l.desc,
                    attribution: s.attribution.clone(),
                    min_z: l.min_z.unwrap_or(s.min_z),
                    opacity: l.opacity,
                    url: s.url.clone(),
                    crs: s.crs,
                    layer: l.layer,
                });
            }
        }
        Catalog { layers, presets: raw.presets }
    })
}

pub fn index_of(id: &str) -> Option<u16> {
    catalog().layers.iter().position(|l| l.id == id).map(|i| i as u16)
}

/// Ostatni błąd usługi na warstwę (do panelu).
fn errors() -> &'static Mutex<HashMap<u16, String>> {
    static E: OnceLock<Mutex<HashMap<u16, String>>> = OnceLock::new();
    E.get_or_init(Default::default)
}

pub fn error_of(i: u16) -> Option<String> {
    errors().lock().unwrap().get(&i).cloned()
}

impl LayerDef {
    fn getmap(&self, t: TileId) -> String {
        let bbox = if self.crs == 4326 {
            // WMS 1.3.0: w EPSG:4326 najpierw szerokość
            let b = t.bbox();
            format!("{:.8},{:.8},{:.8},{:.8}", b.s, b.w, b.n, b.e)
        } else {
            let (x0, y0, x1, y1) = merc_bounds(t);
            format!("{x0:.2},{y0:.2},{x1:.2},{y1:.2}")
        };
        format!(
            "{}?SERVICE=WMS&REQUEST=GetMap&VERSION=1.3.0&LAYERS={}&STYLES=&CRS=EPSG:{}&BBOX={bbox}&WIDTH={TILE_PX}&HEIGHT={TILE_PX}&FORMAT=image/png&TRANSPARENT=TRUE",
            self.url, self.layer, self.crs
        )
    }

    fn legend(&self) -> String {
        format!(
            "{}?SERVICE=WMS&REQUEST=GetLegendGraphic&VERSION=1.3.0&LAYER={}&STYLE=&FORMAT=image/png&SLD_VERSION=1.1.0",
            self.url, self.layer
        )
    }

    fn fetch(&self, t: TileId) -> Result<image::RgbaImage, FetchError> {
        let b = get_bytes(&self.getmap(t), 8 << 20)?;
        // usługi zgłaszają błędy dokumentem XML z kodem 200
        let img = image::load_from_memory(&b)
            .map_err(|_| FetchError::Network("usługa nie zwróciła obrazu".into()))?
            .to_rgba8();
        Ok(if self.crs == 4326 { to_mercator(&img, t) } else { img })
    }
}

/// Obraz w równych stopniach szerokości → wiersze Web Mercatora.
fn to_mercator(src: &image::RgbaImage, t: TileId) -> image::RgbaImage {
    let (w, h) = src.dimensions();
    let b = t.bbox();
    let n = (1u64 << t.z) as f64;
    let mut out = image::RgbaImage::new(w, h);
    for y in 0..h {
        let lat = y_to_lat((t.y as f64 + (y as f64 + 0.5) / h as f64) / n);
        let sy = (((b.n - lat) / (b.n - b.s) * h as f64) as u32).min(h - 1);
        for x in 0..w {
            out.put_pixel(x, y, *src.get_pixel(x, sy));
        }
    }
    out
}

fn tile_path(root: &Path, id: &str, t: TileId) -> PathBuf {
    root.join("overlays").join(id).join(t.z.to_string()).join(format!("{}_{}.png", t.x, t.y))
}

/// Kafel warstwy z dysku albo z usługi; `None` – kafel pusty (zapisany jako plik zerowy).
pub fn load(root: &Path, i: u16, t: TileId) -> Option<image::RgbaImage> {
    if !national::tile_in_poland(t) {
        return None;
    }
    let def = catalog().layers.get(i as usize)?;
    let path = tile_path(root, &def.id, t);
    if let Ok(b) = fs::read(&path) {
        if let Ok(f) = fs::File::options().write(true).open(&path) {
            let _ = f.set_modified(std::time::SystemTime::now());
        }
        return if b.is_empty() { None } else { image::load_from_memory(&b).ok().map(|d| d.to_rgba8()) };
    }
    match def.fetch(t) {
        Ok(img) => {
            errors().lock().unwrap().remove(&i);
            let visible = img.pixels().any(|p| p[3] > 0);
            let mut bytes = Vec::new();
            if visible {
                let _ = image::DynamicImage::ImageRgba8(img.clone())
                    .write_to(&mut std::io::Cursor::new(&mut bytes), image::ImageFormat::Png);
            }
            if let Some(dir) = path.parent() {
                let _ = fs::create_dir_all(dir);
            }
            let tmp = path.with_extension("tmp");
            if fs::write(&tmp, &bytes).is_ok() {
                let _ = fs::rename(&tmp, &path);
            }
            visible.then_some(img)
        }
        Err(FetchError::Missing) => None,
        Err(FetchError::Network(e)) => {
            log::warn!("warstwa {}: {e}", def.id);
            errors().lock().unwrap().insert(i, e);
            None
        }
    }
}

/// Legenda warstwy: RGBA i rozmiar.
pub fn fetch_legend(i: u16) -> Option<image::RgbaImage> {
    let b = get_bytes(&catalog().layers.get(i as usize)?.legend(), 4 << 20).ok()?;
    image::load_from_memory(&b).ok().map(|d| d.to_rgba8())
}

fn presets_file() -> PathBuf {
    crate::settings::config_dir().join("presets.json")
}

/// Własne zestawy użytkownika (`%APPDATA%\PSM\presets.json`).
pub fn load_presets() -> Vec<Preset> {
    fs::read(presets_file()).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
}

pub fn save_presets(list: &[Preset]) -> std::io::Result<()> {
    fs::create_dir_all(crate::settings::config_dir())?;
    fs::write(presets_file(), serde_json::to_vec_pretty(list).unwrap_or_default())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_is_consistent() {
        let c = catalog();
        assert!(c.layers.len() >= 40);
        let ids: std::collections::HashSet<&str> = c.layers.iter().map(|l| l.id.as_str()).collect();
        assert_eq!(ids.len(), c.layers.len());
        for p in &c.presets {
            for l in &p.layers {
                assert!(ids.contains(l.as_str()), "{l}");
            }
        }
    }

    #[test]
    fn getmap_axis_order() {
        let t = crate::geo::tile_at(19.94, 50.06, 12);
        let def = LayerDef {
            id: "x".into(), name: String::new(), group: String::new(), desc: String::new(), attribution: String::new(),
            min_z: 0, opacity: 1.0, url: "https://a/wms".into(), crs: 4326, layer: "L".into(),
        };
        let u = def.getmap(t);
        let bbox = u.split("BBOX=").nth(1).unwrap().split('&').next().unwrap();
        let first: f64 = bbox.split(',').next().unwrap().parse().unwrap();
        assert!((first - 50.0).abs() < 1.0, "{bbox}");
    }
}
