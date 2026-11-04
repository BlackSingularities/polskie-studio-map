//! Dane okolicy kamery: sektory OSM (z dysku albo z Overpass), złożony graf szlaków i punkty.
//! Wszystko w wątku roboczym; interfejs czyta gotową migawkę bez blokowania.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, RwLock};
use std::thread;
use std::time::{Duration, Instant};

use crate::trails::TrailGraph;
use crate::heights::Heights;
use crate::osm::{self, Poi, PoiKind, SectorData, SectorId};

/// Punkt z wysokością i „ważnością” (do rozrzedzania etykiet).
#[derive(Clone, Debug)]
pub struct PoiView {
    pub poi: Poi,
    /// Wysokość do wyświetlania (OSM albo model terenu).
    pub ele: f32,
    /// Wysokość modelu terenu w punkcie.
    pub dem: f32,
    pub prom: f32,
}

pub struct Snapshot {
    pub graph: TrailGraph,
    pub pois: Vec<PoiView>,
    pub sectors: Vec<SectorId>,
    pub version: u64,
}

impl Snapshot {
    fn empty() -> Self {
        Snapshot {
            graph: TrailGraph::empty(),
            pois: Vec::new(),
            sectors: Vec::new(),
            version: 0,
        }
    }
}

#[derive(Default)]
struct State {
    wanted: Vec<SectorId>,
    loaded: HashMap<SectorId, Arc<SectorData>>,
    /// Wczytane na razie same punkty – szlaki jeszcze do pobrania.
    partial: HashSet<SectorId>,
    trails_failed: HashMap<SectorId, Instant>,
    trails_busy: Option<SectorId>,
    /// Rośnie przy każdej zmianie danych sektorów.
    data_gen: u64,
    failed: HashMap<SectorId, Instant>,
    busy: Option<SectorId>,
    error: Option<String>,
    changed: bool,
}

#[derive(Clone)]
pub struct World {
    heights: Heights,
    state: Arc<(Mutex<State>, Condvar)>,
    snap: Arc<RwLock<Arc<Snapshot>>>,
    version: Arc<AtomicU64>,
    /// Rośnie po zapisaniu każdej nowej maski pokrycia terenu.
    land_version: Arc<AtomicU64>,
    /// Pobieranie i wypiekanie maski pokrycia terenu w toku.
    land_busy: Arc<std::sync::atomic::AtomicBool>,
}

const MAX_LOADED: usize = 25;

impl World {
    pub fn new(heights: Heights, ctx: eframe::egui::Context) -> Self {
        let w = World {
            heights,
            state: Arc::new((Mutex::new(State::default()), Condvar::new())),
            snap: Arc::new(RwLock::new(Arc::new(Snapshot::empty()))),
            version: Arc::default(),
            land_version: Arc::default(),
            land_busy: Arc::default(),
        };
        let me = w.clone();
        let c = ctx.clone();
        let c2 = ctx.clone();
        thread::Builder::new()
            .name("okolica".into())
            .spawn(move || me.run(c))
            .expect("wątek okolicy");
        let me = w.clone();
        thread::Builder::new()
            .name("pokrycie".into())
            .spawn(move || me.run_land(ctx))
            .expect("wątek pokrycia terenu");
        let me = w.clone();
        thread::Builder::new()
            .name("szlaki".into())
            .spawn(move || me.run_trails(c2))
            .expect("wątek szlaków");
        w
    }

    pub fn land_busy(&self) -> bool {
        self.land_busy.load(Ordering::Relaxed)
    }

    pub fn land_version(&self) -> u64 {
        self.land_version.load(Ordering::Relaxed)
    }

    /// Maski pokrycia terenu dla wczytanych sektorów okolicy (po kolei, z przerwą po błędzie).
    fn run_land(&self, ctx: eframe::egui::Context) {
        let mut failed: HashMap<SectorId, Instant> = HashMap::new();
        loop {
            thread::sleep(Duration::from_secs(3));
            let root = self.heights.store().root().to_path_buf();
            let next = {
                let s = self.state.0.lock().unwrap();
                s.wanted
                    .iter()
                    .filter(|id| s.loaded.contains_key(id) && !s.partial.contains(id))
                    .find(|id| {
                        !crate::landcover::exists(&root, **id)
                            && failed.get(id).is_none_or(|t| t.elapsed() > Duration::from_secs(300))
                    })
                    .copied()
            };
            let Some(id) = next else { continue };
            self.land_busy.store(true, Ordering::Relaxed);
            ctx.request_repaint();
            let r = crate::landcover::fetch(id).and_then(|img| crate::landcover::save(&root, id, &img));
            self.land_busy.store(false, Ordering::Relaxed);
            match r {
                Ok(()) => {
                    log::info!("pokrycie terenu {}/{} zapisane", id.x, id.y);
                    self.land_version.fetch_add(1, Ordering::Relaxed);
                    ctx.request_repaint();
                }
                Err(e) => {
                    log::warn!("pokrycie terenu {}/{}: {e}", id.x, id.y);
                    failed.insert(id, Instant::now());
                }
            }
        }
    }

    pub fn snapshot(&self) -> Arc<Snapshot> {
        self.snap.read().unwrap().clone()
    }

    /// Zestaw sektorów, które mają być wczytane (okolica kamery).
    /// Kolejność listy to kolejność wczytywania: najpierw to, na co patrzy kamera.
    pub fn want(&self, list: Vec<SectorId>) {
        let mut seen = HashSet::new();
        let list: Vec<SectorId> = list.into_iter().filter(|id| seen.insert(*id)).collect();
        let (m, cv) = &*self.state;
        let mut s = m.lock().unwrap();
        if s.wanted != list {
            s.wanted = list;
            s.changed = true;
            cv.notify_all();
        }
    }

    /// Stan dla interfejsu: (sektor w trakcie pobierania, ostatni błąd).
    pub fn status(&self) -> (Option<SectorId>, Option<String>) {
        let s = self.state.0.lock().unwrap();
        (s.busy.or(s.trails_busy), s.error.clone())
    }

    /// Po zmianie folderu danych: wszystko od nowa.
    pub fn reset(&self) {
        let (m, cv) = &*self.state;
        let mut s = m.lock().unwrap();
        s.loaded.clear();
        s.partial.clear();
        s.failed.clear();
        s.trails_failed.clear();
        s.data_gen += 1;
        s.changed = true;
        cv.notify_all();
    }

    fn run(&self, ctx: eframe::egui::Context) {
        let mut built_gen = 0;
        loop {
            // następne zadanie: brakujący sektor z okolicy (szlaki ma osobny wątek)
            let (todo, rebuild) = {
                let (m, cv) = &*self.state;
                let mut s = m.lock().unwrap();
                loop {
                    if s.changed {
                        s.changed = false;
                        break (None, true);
                    }
                    let next = s
                        .wanted
                        .iter()
                        .find(|id| {
                            !s.loaded.contains_key(id)
                                && s.failed.get(id).is_none_or(|t| t.elapsed() > Duration::from_secs(120))
                        })
                        .copied();
                    if let Some(id) = next {
                        break (Some(id), false);
                    }
                    s = cv.wait_timeout(s, Duration::from_secs(30)).unwrap().0;
                }
            };
            if let Some(id) = todo {
                self.state.0.lock().unwrap().busy = Some(id);
                ctx.request_repaint();
                self.load_sector(id);
                let mut s = self.state.0.lock().unwrap();
                s.busy = None;
                // nadmiar wczytanych: zostają te z okolicy
                if s.loaded.len() > MAX_LOADED {
                    let keep: HashSet<SectorId> = s.wanted.iter().copied().collect();
                    s.loaded.retain(|k, _| keep.contains(k));
                    s.partial.retain(|k| keep.contains(k));
                }
                drop(s);
                ctx.request_repaint();
                continue;
            }
            if rebuild {
                let (mut sectors, gen): (Vec<(SectorId, Arc<SectorData>)>, u64) = {
                    let s = self.state.0.lock().unwrap();
                    let list = s
                        .wanted
                        .iter()
                        .filter_map(|id| s.loaded.get(id).map(|d| (*id, d.clone())))
                        .collect();
                    (list, s.data_gen)
                };
                // ten sam zestaw w innej kolejności to te same dane
                sectors.sort_by_key(|s| s.0);
                let current: Vec<SectorId> = sectors.iter().map(|s| s.0).collect();
                if current == self.snapshot().sectors && gen == built_gen {
                    continue;
                }
                built_gen = gen;
                let t0 = Instant::now();
                let snap = self.build(&sectors);
                log::info!(
                    "okolica: {} sektorów, {} km szlaków, {} punktów ({:.0} ms)",
                    snap.sectors.len(),
                    snap.graph.total_km as u64,
                    snap.pois.len(),
                    t0.elapsed().as_secs_f64() * 1000.0
                );
                *self.snap.write().unwrap() = Arc::new(snap);
                ctx.request_repaint();
            }
        }
    }

    /// Wczytanie sektora: z dysku, a gdy brak – same punkty, od razu widoczne.
    fn load_sector(&self, id: SectorId) {
        let root = self.heights.store().root().to_path_buf();
        let result = match osm::load(&root, id) {
            Some(d) => Ok((d, false)),
            None => osm::fetch_pois(id).map(|p| (osm::partial(p), true)),
        };
        let (m, cv) = &*self.state;
        let mut s = m.lock().unwrap();
        match result {
            Ok((d, is_partial)) => {
                s.error = None;
                s.failed.remove(&id);
                s.loaded.insert(id, Arc::new(d));
                if is_partial {
                    s.partial.insert(id);
                }
                s.data_gen += 1;
                s.changed = true;
                cv.notify_all();
            }
            Err(e) => {
                log::warn!("sektor {}/{}: {e}", id.x, id.y);
                s.error = Some(e);
                s.failed.insert(id, Instant::now());
            }
        }
    }

    /// Szlaki sektorów z okolicy, po kolei: Overpass bywa wolny i nie może blokować reszty.
    fn run_trails(&self, ctx: eframe::egui::Context) {
        loop {
            let next = {
                let mut s = self.state.0.lock().unwrap();
                let id = s
                    .wanted
                    .iter()
                    .find(|id| {
                        s.partial.contains(id)
                            && s.trails_failed.get(id).is_none_or(|t| t.elapsed() > Duration::from_secs(300))
                    })
                    .copied();
                s.trails_busy = id;
                id.and_then(|id| s.loaded.get(&id).map(|d| (id, d.pois.clone())))
            };
            let Some((id, pois)) = next else {
                thread::sleep(Duration::from_secs(1));
                continue;
            };
            ctx.request_repaint();
            let root = self.heights.store().root().to_path_buf();
            let result = osm::fetch_trails(id, pois);
            let (m, cv) = &*self.state;
            let mut s = m.lock().unwrap();
            s.trails_busy = None;
            match result {
                Ok(d) => {
                    if let Err(e) = osm::save(&root, id, &d) {
                        log::error!("zapis sektora: {e}");
                    }
                    s.error = None;
                    s.trails_failed.remove(&id);
                    s.loaded.insert(id, Arc::new(d));
                    s.partial.remove(&id);
                    s.data_gen += 1;
                    s.changed = true;
                    cv.notify_all();
                }
                Err(e) => {
                    log::warn!("szlaki {}/{}: {e}", id.x, id.y);
                    s.error = Some(e);
                    s.trails_failed.insert(id, Instant::now());
                }
            }
            drop(s);
            ctx.request_repaint();
        }
    }

    fn build(&self, sectors: &[(SectorId, Arc<SectorData>)]) -> Snapshot {
        let h = self.heights.clone();
        let height = move |lon: f64, lat: f64| h.sample(lon, lat).unwrap_or(0.0);
        let refs: Vec<&SectorData> = sectors.iter().map(|s| &*s.1).collect();
        let graph = TrailGraph::build(&refs, &height);
        use rayon::prelude::*;
        let raw: Vec<&Poi> = refs
            .iter()
            .flat_map(|s| s.pois.iter())
            .filter(|p| crate::national::in_poland(p.lon, p.lat))
            .collect();
        let mut pois: Vec<PoiView> = raw
            .par_iter()
            .map(|p| {
                let dem = height(p.lon, p.lat);
                PoiView {
                    poi: (*p).clone(),
                    ele: p.ele.unwrap_or(dem),
                    dem,
                    prom: 0.0,
                }
            })
            .collect();
        prominence(&mut pois, &height);
        pois.sort_by(|a, b| b.prom.total_cmp(&a.prom));
        Snapshot {
            graph,
            pois,
            sectors: sectors.iter().map(|s| s.0).collect(),
            version: self.version.fetch_add(1, Ordering::Relaxed) + 1,
        }
    }
}

/// Przybliżona „ważność” punktu: izolacja szczytu i wybitność lokalna, typ miejsca.
fn prominence(pois: &mut [PoiView], height: &(dyn Fn(f64, f64) -> f32 + Sync)) {
    use rayon::prelude::*;
    let peaks: Vec<(f64, f64, f32)> = pois
        .iter()
        .filter(|p| p.poi.kind == PoiKind::Peak)
        .map(|p| (p.poi.lon, p.poi.lat, p.ele))
        .collect();
    pois.par_iter_mut().for_each(|p| {
        p.prom = match p.poi.kind {
            PoiKind::Peak => {
                let kx = 111_320.0 * p.poi.lat.to_radians().cos();
                let mut iso = 30_000.0f64;
                for &(lon, lat, e) in &peaks {
                    if e > p.ele {
                        let d = ((lon - p.poi.lon) * kx).hypot((lat - p.poi.lat) * 110_574.0);
                        iso = iso.min(d);
                    }
                }
                let mut low = p.ele;
                for a in 0..16 {
                    let ang = a as f64 / 16.0 * std::f64::consts::TAU;
                    let (dx, dy) = (ang.cos() * 600.0, ang.sin() * 600.0);
                    low = low.min(height(p.poi.lon + dx / kx, p.poi.lat + dy / 110_574.0));
                }
                (iso / 1000.0) as f32 + (p.ele - low) / 120.0
            }
            PoiKind::Hut => 6.0,
            PoiKind::Lake => 3.0,
            PoiKind::Saddle => 1.2,
            PoiKind::Place => {
                let base = match p.poi.place.as_str() {
                    "city" => 16.0,
                    "town" => 9.0,
                    "village" => 2.6,
                    "hamlet" => 0.7,
                    _ => 1.0,
                };
                base + (p.poi.pop.max(1) as f32).log10()
                    * if p.poi.place == "village" { 0.35 } else { 0.2 }
            }
            _ => 0.8,
        };
    });
}
