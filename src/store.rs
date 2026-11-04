//! Magazyn danych map na dysku: jeden folder wybrany przez użytkownika.
//!
//! ```text
//! <folder>\PSM-dane\
//!   tiles\dem\z\bx_by.gpk         teren (wypieczony: u16 + LZ4), paczki 16×16 kafli
//!   tiles\sat\z\bx_by.gpk         obraz satelitarny (JPEG), paczki 16×16 kafli
//!   tiles\ortho\z\bx_by.gpk       ortofotomapa (JPEG)
//!   overlays\<warstwa>\z\x_y.png  warstwy tematyczne (WMS)
//!   sectors\, land\                szlaki, nazwy i pokrycie terenu z OSM
//! ```
//!
//! Wszystko zapisuje się samo w miarę oglądania. Po przekroczeniu limitu sprzątacz usuwa paczki
//! od najdawniej używanych; model globalny do poziomu [`BASE_MAX_Z`] zostaje.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Instant, SystemTime};

use crate::geo::TileId;
use crate::pack::{self, PackKey, Packs, Slot};

/// Model globalny do tego poziomu nigdy nie jest usuwany (cała Europa to kilkadziesiąt MB).
pub const BASE_MAX_Z: u8 = 6;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Layer {
    /// Wysokości terenu (Mapterhorn: krajowe modele LiDAR; AWS Open Data jako zapas).
    Dem,
    /// Obraz satelitarny (Sentinel-2 cloudless 2020 © EOX, CC BY-NC-SA 4.0).
    Sat,
    /// Ortofotomapa GUGiK na tle Sentinel-2, JPEG 512 px.
    Ortho,
}

impl Layer {
    /// Warstwy z własnymi paczkami na dysku.
    pub const STORED: [Layer; 3] = [Layer::Dem, Layer::Sat, Layer::Ortho];

    pub fn id(self) -> u8 {
        match self {
            Layer::Dem => 0,
            Layer::Sat => 1,
            Layer::Ortho => 2,
        }
    }

    pub fn dir(self) -> &'static str {
        match self {
            Layer::Dem => "dem",
            Layer::Sat => "sat",
            // „ortho2”: kafle przycięte do granicy kraju
            Layer::Ortho => "ortho2",
        }
    }

    pub fn url(self, t: TileId) -> String {
        match self {
            Layer::Dem => format!(
                "https://s3.amazonaws.com/elevation-tiles-prod/terrarium/{}/{}/{}.png",
                t.z, t.x, t.y
            ),
            Layer::Sat => format!(
                "https://tiles.maps.eox.at/wmts/1.0.0/s2cloudless-2020_3857/default/g/{}/{}/{}.jpg",
                t.z, t.y, t.x
            ),
            // składane z kilku usług (`national`)
            Layer::Ortho => String::new(),
        }
    }

    /// Najwyższy poziom dostępny w źródle.
    pub fn max_z(self) -> u8 {
        15
    }
}

/// Dostęp do danych: tani do klonowania (wspólne paczki i pamięć otwartych plików).
#[derive(Clone)]
pub struct Store {
    root: PathBuf,
    packs: Arc<Packs>,
    touched: Arc<Mutex<HashMap<PackKey, Instant>>>,
}

impl Store {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        let root = root.into();
        Self {
            packs: Arc::new(Packs::new(root.join("tiles"))),
            root,
            touched: Arc::default(),
        }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Kafel jest na dysku albo wiadomo, że źródło go nie ma.
    pub fn has(&self, layer: Layer, t: TileId) -> bool {
        self.packs.state(layer.id(), layer.dir(), t) != Slot::Empty
    }

    /// Wypieczone dane kafla.
    pub fn read(&self, layer: Layer, t: TileId) -> Option<Vec<u8>> {
        let b = self.packs.read(layer.id(), layer.dir(), t)?;
        self.touch(layer, t);
        Some(b)
    }

    /// Zapis wypieczonego kafla; `None` = źródło nie ma tego kafla.
    pub fn write(&self, layer: Layer, t: TileId, baked: Option<&[u8]>) -> std::io::Result<()> {
        self.packs.write(layer.id(), layer.dir(), t, baked)
    }

    /// Oznaczenie użycia paczki (czas modyfikacji = ostatnie użycie), najwyżej raz na minutę.
    fn touch(&self, layer: Layer, t: TileId) {
        let (key, _) = PackKey::of(layer.id(), t);
        let mut map = self.touched.lock().unwrap();
        if map.get(&key).is_some_and(|at| at.elapsed().as_secs() < 60) {
            return;
        }
        map.insert(key, Instant::now());
        if let Ok(f) = fs::File::options()
            .write(true)
            .open(self.packs.path(&key, layer.dir()))
        {
            let _ = f.set_modified(SystemTime::now());
        }
    }

    pub fn usage(&self) -> u64 {
        dir_size(&self.root)
    }

    /// Sprzątanie do limitu: usuwa najdawniej używane paczki (model globalny zostaje).
    /// Zwraca (liczba usuniętych paczek, zwolnione bajty).
    pub fn collect_garbage(&self, limit: u64) -> (usize, u64) {
        let mut total = self.usage();
        if total <= limit {
            return (0, 0);
        }
        let mut free: Vec<(SystemTime, u64, Option<PackKey>, PathBuf)> = Vec::new();
        for layer in Layer::STORED {
            for (key, path) in pack::list(&self.root.join("tiles"), layer.id(), layer.dir()) {
                if key.z <= BASE_MAX_Z {
                    continue;
                }
                if let Ok(m) = fs::metadata(&path) {
                    free.push((
                        m.modified().unwrap_or(SystemTime::UNIX_EPOCH),
                        m.len(),
                        Some(key),
                        path,
                    ));
                }
            }
        }
        list_files(&self.root.join("overlays"), &mut free);
        free.sort_by_key(|f| f.0);
        let (mut n, mut freed) = (0, 0);
        for (_, size, key, path) in free {
            if total <= limit * 9 / 10 {
                break;
            }
            if let Some(key) = &key {
                self.packs.forget(key);
            }
            if fs::remove_file(&path).is_ok() {
                total = total.saturating_sub(size);
                freed += size;
                n += 1;
            }
        }
        (n, freed)
    }
}

fn list_files(dir: &Path, out: &mut Vec<(SystemTime, u64, Option<PackKey>, PathBuf)>) {
    let Ok(rd) = fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        match e.metadata() {
            Ok(m) if m.is_dir() => list_files(&e.path(), out),
            Ok(m) => out.push((m.modified().unwrap_or(SystemTime::UNIX_EPOCH), m.len(), None, e.path())),
            Err(_) => {}
        }
    }
}

pub fn dir_size(dir: &Path) -> u64 {
    let Ok(rd) = fs::read_dir(dir) else { return 0 };
    rd.flatten()
        .map(|e| match e.file_type() {
            Ok(t) if t.is_dir() => dir_size(&e.path()),
            _ => e.metadata().map(|m| m.len()).unwrap_or(0),
        })
        .sum()
}

/// Wolne miejsce na dysku, na którym leży `dir` (pierwszy istniejący katalog nadrzędny).
pub fn free_space(dir: &Path) -> Option<u64> {
    let mut d = dir;
    while !d.exists() {
        d = d.parent()?;
    }
    free_space_impl(d)
}

#[cfg(windows)]
fn free_space_impl(dir: &Path) -> Option<u64> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::GetDiskFreeSpaceExW;
    let wide: Vec<u16> = dir.as_os_str().encode_wide().chain(Some(0)).collect();
    let mut avail = 0u64;
    // SAFETY: ścieżka zakończona zerem, wskaźniki na lokalne zmienne lub null
    let ok = unsafe {
        GetDiskFreeSpaceExW(
            wide.as_ptr(),
            &mut avail,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    };
    (ok != 0).then_some(avail)
}

#[cfg(not(windows))]
fn free_space_impl(_dir: &Path) -> Option<u64> {
    None
}

/// „12,4 GB”, „380 MB”.
pub fn fmt_bytes(b: u64) -> String {
    const G: f64 = 1024.0 * 1024.0 * 1024.0;
    let f = b as f64;
    if f >= 1024.0 * G {
        format!("{:.1} TB", f / 1024.0 / G).replace('.', ",")
    } else if f >= 10.0 * G {
        format!("{:.0} GB", f / G)
    } else if f >= G {
        format!("{:.1} GB", f / G).replace('.', ",")
    } else {
        format!("{:.0} MB", f / 1024.0 / 1024.0)
    }
}
