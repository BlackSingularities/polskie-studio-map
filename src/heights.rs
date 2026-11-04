//! Wysokości terenu: wspólna pamięć zdekodowanych kafli DEM (dla renderera, szlaków i analiz)
//! oraz próbkowanie w dowolnym punkcie z najlepszego dostępnego poziomu.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, RwLock};

use crate::bake::{decode_dem, Dem};
use crate::download;
use crate::geo::{lat_to_y, lon_to_x, meters_per_pixel, BBox, TileId};
use crate::store::{Layer, Store};

/// Najwyższy poziom terenu w źródłach.
pub const MAX_Z: u8 = 16;
/// Poziom pobierany do analiz i szlaków, gdy na dysku nie ma nic dokładniejszego (~25 m).
pub const ANALYSIS_Z: u8 = 12;

const CAPACITY: usize = 700;

struct Entry {
    dem: Arc<Dem>,
    used: u64,
}

#[derive(Default)]
struct Inner {
    tiles: HashMap<TileId, Entry>,
    /// Kafle, których nie ma na dysku ani w źródle (żeby nie pytać w kółko).
    absent: HashMap<TileId, bool>,
    clock: u64,
}

/// Tani do klonowania dostęp do wysokości.
#[derive(Clone)]
pub struct Heights {
    store: Arc<RwLock<Store>>,
    inner: Arc<Mutex<Inner>>,
}

impl Heights {
    pub fn new(store: Store) -> Self {
        Self {
            store: Arc::new(RwLock::new(store)),
            inner: Arc::default(),
        }
    }

    pub fn set_store(&self, store: Store) {
        *self.store.write().unwrap() = store;
        *self.inner.lock().unwrap() = Inner::default();
    }

    pub fn store(&self) -> Store {
        self.store.read().unwrap().clone()
    }

    /// Kafel z pamięci (bez czytania dysku).
    pub fn cached(&self, t: TileId) -> Option<Arc<Dem>> {
        let mut g = self.inner.lock().unwrap();
        g.clock += 1;
        let c = g.clock;
        g.tiles.get_mut(&t).map(|e| {
            e.used = c;
            e.dem.clone()
        })
    }

    fn insert(&self, t: TileId, dem: Arc<Dem>) {
        let mut g = self.inner.lock().unwrap();
        g.clock += 1;
        let c = g.clock;
        g.tiles.insert(t, Entry { dem, used: c });
        if g.tiles.len() > CAPACITY {
            let mut ages: Vec<u64> = g.tiles.values().map(|e| e.used).collect();
            ages.sort_unstable();
            let cut = ages[ages.len() - CAPACITY * 3 / 4];
            g.tiles.retain(|_, e| e.used >= cut);
        }
    }

    /// Kafel z dysku (bez sieci).
    pub fn local(&self, t: TileId) -> Option<Arc<Dem>> {
        if let Some(d) = self.cached(t) {
            return Some(d);
        }
        if self.inner.lock().unwrap().absent.contains_key(&t) {
            return None;
        }
        let store = self.store();
        match store.read(download::dem_layer(t), t).and_then(|b| decode_dem(&b)) {
            Some(d) => {
                let d = Arc::new(d);
                self.insert(t, d.clone());
                Some(d)
            }
            None => {
                self.inner.lock().unwrap().absent.insert(t, false);
                None
            }
        }
    }

    /// Kafel z dysku albo z sieci (blokujące – tylko w wątkach roboczych).
    pub fn load(&self, t: TileId) -> Option<Arc<Dem>> {
        if let Some(d) = self.local(t) {
            return Some(d);
        }
        if t.z > download::TERRAIN_MAX_Z {
            return None;
        }
        if self.inner.lock().unwrap().absent.get(&t) == Some(&true) {
            return None;
        }
        let store = self.store();
        match download::get(&store, Layer::Dem, t)
            .ok()
            .and_then(|b| decode_dem(&b))
        {
            Some(d) => {
                let d = Arc::new(d);
                self.inner.lock().unwrap().absent.remove(&t);
                self.insert(t, d.clone());
                Some(d)
            }
            None => {
                // brak w źródle albo sieci – zapamiętane na tę sesję
                self.inner.lock().unwrap().absent.insert(t, true);
                None
            }
        }
    }

    /// Wysokość w punkcie: najdokładniejsze dane na dysku, a gdy brak – pobrany kafel `ANALYSIS_Z`.
    /// Bliskie poziomy tylko z dysku: wolne usługi LiDAR nie mogą wstrzymywać szlaków i nazw.
    pub fn sample(&self, lon: f64, lat: f64) -> Option<f32> {
        (ANALYSIS_Z..=MAX_Z)
            .rev()
            .find_map(|z| self.sample_with(lon, lat, z, |t| self.local(t)))
            .or_else(|| self.sample_at(lon, lat, ANALYSIS_Z, true))
            .or_else(|| self.sample_at(lon, lat, 10, true))
    }

    /// Wysokość tylko z pamięci (wątek interfejsu): najdokładniejszy wczytany poziom.
    pub fn sample_cached(&self, lon: f64, lat: f64) -> Option<f32> {
        (6..=MAX_Z)
            .rev()
            .find_map(|z| self.sample_at(lon, lat, z, false))
    }

    /// Dwuliniowe próbkowanie na poziomie `z` (piksele sąsiednich kafli też, jeśli potrzeba).
    pub fn sample_at(&self, lon: f64, lat: f64, z: u8, load: bool) -> Option<f32> {
        self.sample_with(lon, lat, z, |t| if load { self.load(t) } else { self.cached(t) })
    }

    fn sample_with(&self, lon: f64, lat: f64, z: u8, tile: impl Fn(TileId) -> Option<Arc<Dem>>) -> Option<f32> {
        let n = (1u64 << z) as f64 * 256.0;
        let px = lon_to_x(lon) * n - 0.5;
        let py = lat_to_y(lat) * n - 0.5;
        let (x0, y0) = (px.floor(), py.floor());
        let (fx, fy) = ((px - x0) as f32, (py - y0) as f32);
        let mut v = [0f32; 4];
        let mut last: Option<(TileId, Arc<Dem>)> = None;
        for (k, (dx, dy)) in [(0.0, 0.0), (1.0, 0.0), (0.0, 1.0), (1.0, 1.0)]
            .into_iter()
            .enumerate()
        {
            let gx = (x0 + dx).clamp(0.0, n - 1.0) as u64;
            let gy = (y0 + dy).clamp(0.0, n - 1.0) as u64;
            let t = TileId::new(z, (gx / 256) as u32, (gy / 256) as u32);
            let dem = match &last {
                Some((lt, d)) if *lt == t => d.clone(),
                _ => {
                    let d = tile(t)?;
                    last = Some((t, d.clone()));
                    d
                }
            };
            v[k] = dem_px(&dem, (gx % 256) as usize, (gy % 256) as usize);
        }
        Some((v[0] * (1.0 - fx) + v[1] * fx) * (1.0 - fy) + (v[2] * (1.0 - fx) + v[3] * fx) * fy)
    }

    /// Siatka wysokości prostokąta na poziomie `z` (do analiz). Blokujące.
    pub fn grid(&self, bbox: &BBox, z: u8) -> Grid {
        let n = (1u64 << z) as f64 * 256.0;
        let px0 = (lon_to_x(bbox.w) * n).floor();
        let py0 = (lat_to_y(bbox.n) * n).floor();
        let px1 = (lon_to_x(bbox.e) * n).ceil();
        let py1 = (lat_to_y(bbox.s) * n).ceil();
        let (w, h) = ((px1 - px0).max(2.0) as usize, (py1 - py0).max(2.0) as usize);
        let mut data = vec![0f32; w * h];
        let (tx0, ty0) = ((px0 / 256.0) as u32, (py0 / 256.0) as u32);
        let (tx1, ty1) = (((px1 - 1.0) / 256.0) as u32, ((py1 - 1.0) / 256.0) as u32);
        let tiles: Vec<TileId> = (ty0..=ty1)
            .flat_map(|y| (tx0..=tx1).map(move |x| TileId::new(z, x, y)))
            .collect();
        use rayon::prelude::*;
        let loaded: Vec<(TileId, Option<Arc<Dem>>)> =
            tiles.par_iter().map(|t| (*t, self.load(*t))).collect();
        for (t, dem) in loaded {
            for ly in 0..256usize {
                let gy = t.y as f64 * 256.0 + ly as f64 - py0;
                if gy < 0.0 || gy >= h as f64 {
                    continue;
                }
                for lx in 0..256usize {
                    let gx = t.x as f64 * 256.0 + lx as f64 - px0;
                    if gx < 0.0 || gx >= w as f64 {
                        continue;
                    }
                    let v = match &dem {
                        Some(d) => dem_px(d, lx, ly),
                        // brak kafla (np. poza zasięgiem danych): zastępczo poziom niżej
                        None => {
                            let lon = crate::geo::x_to_lon((t.x as f64 * 256.0 + lx as f64 + 0.5) / n);
                            let lat = crate::geo::y_to_lat((t.y as f64 * 256.0 + ly as f64 + 0.5) / n);
                            (1..=4)
                                .find_map(|dz| self.sample_at(lon, lat, z.saturating_sub(dz), true))
                                .unwrap_or(0.0)
                        }
                    };
                    data[gy as usize * w + gx as usize] = v;
                }
            }
        }
        let lat = crate::geo::y_to_lat((py0 + h as f64 / 2.0) / n);
        Grid {
            w,
            h,
            z,
            px0,
            py0,
            mpp: meters_per_pixel(lat, z) as f32,
            data,
        }
    }
}

fn dem_px(d: &Dem, x: usize, y: usize) -> f32 {
    if d.w == 256 && d.h == 256 {
        d.elev[y * 256 + x]
    } else {
        let xx = (x * d.w / 256).min(d.w - 1);
        let yy = (y * d.h / 256).min(d.h - 1);
        d.elev[yy * d.w + xx]
    }
}

/// Lokalna siatka wysokości (piksele Web Mercator poziomu `z`).
#[derive(Clone)]
pub struct Grid {
    pub w: usize,
    pub h: usize,
    pub z: u8,
    /// Położenie lewego górnego piksela w pikselach świata poziomu `z`.
    pub px0: f64,
    pub py0: f64,
    /// Metry na piksel (dla środka siatki).
    pub mpp: f32,
    pub data: Vec<f32>,
}

impl Grid {
    fn world_px(&self) -> f64 {
        (1u64 << self.z) as f64 * 256.0
    }

    /// Długość i szerokość → współrzędne siatki (środki pikseli w liczbach całkowitych).
    pub fn to_grid(&self, lon: f64, lat: f64) -> (f64, f64) {
        let n = self.world_px();
        (
            lon_to_x(lon) * n - self.px0 - 0.5,
            lat_to_y(lat) * n - self.py0 - 0.5,
        )
    }

    pub fn to_lonlat(&self, gx: f64, gy: f64) -> (f64, f64) {
        let n = self.world_px();
        (
            crate::geo::x_to_lon((gx + 0.5 + self.px0) / n),
            crate::geo::y_to_lat((gy + 0.5 + self.py0) / n),
        )
    }

    pub fn at(&self, x: usize, y: usize) -> f32 {
        self.data[y.min(self.h - 1) * self.w + x.min(self.w - 1)]
    }

    pub fn sample(&self, gx: f64, gy: f64) -> f32 {
        let x = gx.clamp(0.0, self.w as f64 - 1.001);
        let y = gy.clamp(0.0, self.h as f64 - 1.001);
        let (x0, y0) = (x as usize, y as usize);
        let (fx, fy) = ((x - x0 as f64) as f32, (y - y0 as f64) as f32);
        let i = y0 * self.w + x0;
        let d = &self.data;
        (d[i] * (1.0 - fx) + d[i + 1] * fx) * (1.0 - fy)
            + (d[i + self.w] * (1.0 - fx) + d[i + self.w + 1] * fx) * fy
    }

    pub fn inside(&self, gx: f64, gy: f64) -> bool {
        gx >= 0.0 && gy >= 0.0 && gx <= self.w as f64 - 1.0 && gy <= self.h as f64 - 1.0
    }

    /// Prostokąt geograficzny siatki.
    pub fn bbox(&self) -> BBox {
        let (w, n) = self.to_lonlat(-0.5, -0.5);
        let (e, s) = self.to_lonlat(self.w as f64 - 0.5, self.h as f64 - 0.5);
        BBox::new(w, s, e, n)
    }
}
