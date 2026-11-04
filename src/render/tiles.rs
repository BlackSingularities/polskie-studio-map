//! Teren kaflowy z poziomami szczegółów: drzewo czwórkowe Web Mercator wybierane co klatkę
//! z błędu ekranowego, kafle DEM i zdjęć wczytywane w wątkach (dysk, a gdy brak – sieć),
//! a do czasu wczytania – fragmenty kafli nadrzędnych (bez dziur w obrazie).

use std::collections::{HashMap, HashSet};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Condvar, Mutex};
use std::thread;

use glam::{DVec3, Vec4};

use crate::bake::{self, Dem};
use crate::download;
use crate::geo::{meters_per_pixel, TileId, EARTH_R};
use crate::heights::Heights;
use crate::layers;
use crate::national;
use crate::render::camera::View;
use crate::store::{Layer, Store};

/// Siatka 257×257 narożników pikseli kafla.
pub const DEM_RES: u32 = 257;
/// Najwyższy poziom drzewa (dalej tylko powiększenie danych).
pub const MAX_RENDER_Z: u8 = 17;
/// Najwyższy poziom zdjęć satelitarnych w źródle.
pub const SAT_MAX_Z: u8 = 14;
/// Ortofoto pobierane tylko do tylu kafli swojego poziomu od kamery…
const ORTHO_TILES_RADIUS: f64 = 6.0;
/// …i nie dalej niż tyle metrów.
const ORTHO_MAX_DIST: f64 = 12_000.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Kind {
    Dem,
    Sat,
    /// Ortofotomapa GUGiK (z13–17).
    Ortho,
    /// Ortofoto złożone z kafli już zapisanych na dysku (bez sieci) – tylko w kolejce,
    /// wynik trafia pod `Ortho`.
    OrthoLocal,
    /// Warstwa tematyczna (indeks w katalogu).
    Overlay(u16),
}

/// Najniższy poziom ortofoto złożonego z zapisanych kafli (przy oddalaniu).
const ORTHO_LOCAL_MIN_Z: u8 = 12;
/// O ile poziomów w dół szukać zapisanych kafli do złożenia (z12 ← z15).
const ORTHO_LOCAL_DEPTH: u8 = 3;
/// Po nieudanym złożeniu ponowna próba po tylu klatkach (mogły dojść nowe kafle).
const ORTHO_LOCAL_RETRY: u64 = 600;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct Key(Kind, TileId);

struct Request {
    key: Key,
    prio: f32,
    /// Przeliczenie brzegów – zostaje w kolejce mimo wymiany zleceń.
    sticky: bool,
}

enum Payload {
    Dem {
        heights: Vec<f32>,
        grad: Vec<half::f16>,
        min: f32,
        max: f32,
        /// Brzegi uzgodnione z sąsiadami: [lewy, prawy, górny, dolny].
        seams: [bool; 4],
    },
    Img(Vec<Vec<u8>>),
    Missing,
}

struct Loaded {
    key: Key,
    payload: Payload,
}

struct GpuDem {
    bind: wgpu::BindGroup,
    min: f32,
    max: f32,
    seams: [bool; 4],
    used: u64,
}

struct GpuImg {
    bind: wgpu::BindGroup,
    used: u64,
}

/// Wspólna kolejka zleceń (wątki biorą najważniejsze).
#[derive(Default)]
struct Queue {
    list: Vec<Request>,
    inflight: HashSet<Key>,
    /// Wątki zajęte wolnymi usługami krajowymi.
    slow: usize,
}

/// Kafel do narysowania w tej klatce.
#[derive(Clone, Copy, Debug)]
pub struct DrawTile {
    pub tile: TileId,
    pub dem: TileId,
    pub dem_xform: [f32; 3],
    pub img: Option<((Kind, TileId), [f32; 3])>,
    pub rect: [f32; 4],
}

/// Warstwa tematyczna na kaflu: rysowana osobno, półprzezroczyście.
#[derive(Clone, Copy, Debug)]
pub struct OverlayDraw {
    pub tile: usize,
    pub image: (Kind, TileId),
    pub xform: [f32; 3],
    pub opacity: f32,
}

pub struct TileSettings {
    /// Docelowy rozmiar kafla na ekranie (px) – mniejszy = więcej szczegółów.
    pub target_px: f32,
    pub max_dem: usize,
    pub max_img: usize,
    pub imagery: bool,
    /// Ortofoto GUGiK na bliskich kaflach.
    pub ortho: bool,
    pub max_dist: f64,
    /// Włączone warstwy od spodu: (indeks w katalogu, krycie).
    pub overlays: Vec<(u16, f32)>,
}

pub struct Tiles {
    heights: Heights,
    queue: Arc<(Mutex<Queue>, Condvar)>,
    rx: Receiver<Loaded>,
    dem: HashMap<TileId, GpuDem>,
    img: HashMap<(Kind, TileId), GpuImg>,
    /// Czy kafel leży w zasięgu ortofoto (pamięć wyników – test wielokątów kraju).
    ortho_memo: HashMap<TileId, bool>,
    missing: HashSet<Key>,
    /// Nieudane składanie ortofoto z dysku: klatka próby.
    local_miss: HashMap<TileId, u64>,
    frame: u64,
    pub dem_layout: wgpu::BindGroupLayout,
    pub img_layout: wgpu::BindGroupLayout,
    pub empty_img: wgpu::BindGroup,
    /// Wysokości wczytanych kafli (do granic brył i wybierania punktów).
    pub loading: usize,
    /// W kolejce i w toku: (teren, zdjęcia satelitarne, ortofoto pobierane, ortofoto składane z dysku).
    pub loading_kinds: [usize; 4],
    pub overlay_draws: Vec<OverlayDraw>,
    /// Kafle warstwy w kolejce i narysowane w tej klatce.
    pub layer_loading: HashMap<u16, usize>,
    pub layer_drawn: HashMap<u16, usize>,
}

const WORKERS: usize = 16;
/// Tyle wątków może czekać na usługi krajowe; reszta zawsze obsługuje szybkie źródła.
const SLOW_WORKERS: usize = 6;

impl Tiles {
    pub fn new(device: &wgpu::Device, queue_gpu: &wgpu::Queue, heights: Heights, ctx: eframe::egui::Context) -> Self {
        let tex_entry = |binding, filterable| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
            ty: wgpu::BindingType::Texture {
                sample_type: wgpu::TextureSampleType::Float { filterable },
                view_dimension: wgpu::TextureViewDimension::D2,
                multisampled: false,
            },
            count: None,
        };
        let dem_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("dem"),
            entries: &[tex_entry(0, false), tex_entry(1, true)],
        });
        let img_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("img"),
            entries: &[tex_entry(0, true)],
        });
        let empty = make_texture(device, queue_gpu, wgpu::TextureFormat::Rgba8UnormSrgb, 1, 1, &[vec![128, 128, 128, 255]], 4);
        let empty_img = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("img-empty"),
            layout: &img_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(&empty.create_view(&Default::default())),
            }],
        });
        let queue = Arc::new((Mutex::new(Queue::default()), Condvar::new()));
        let (tx, rx) = channel();
        for i in 0..WORKERS {
            let (q, tx, h, ctx) = (queue.clone(), tx.clone(), heights.clone(), ctx.clone());
            thread::Builder::new()
                .name(format!("kafle-{i}"))
                .spawn(move || worker(q, tx, h, ctx))
                .expect("wątek kafli");
        }
        Tiles {
            heights,
            queue,
            rx,
            dem: HashMap::new(),
            img: HashMap::new(),
            ortho_memo: HashMap::new(),
            missing: HashSet::new(),
            local_miss: HashMap::new(),
            frame: 0,
            dem_layout,
            img_layout,
            empty_img,
            loading: 0,
            loading_kinds: [0; 4],
            overlay_draws: Vec::new(),
            layer_loading: HashMap::new(),
            layer_drawn: HashMap::new(),
        }
    }

    /// Po zmianie folderu danych.
    pub fn reset(&mut self) {
        self.dem.clear();
        self.img.clear();
        self.missing.clear();
        self.local_miss.clear();
        let mut q = self.queue.0.lock().unwrap();
        q.list.clear();
    }

    pub fn resident(&self) -> (usize, usize) {
        (self.dem.len(), self.img.len())
    }

    /// Wysokość z rezydentnego kafla (ten sam model, który widać na ekranie).
    pub fn dem_minmax(&self, t: TileId) -> Option<(f32, f32)> {
        let mut t = t;
        loop {
            if let Some(d) = self.dem.get(&t) {
                return Some((d.min, d.max));
            }
            if t.z == 0 {
                return None;
            }
            t = t.ancestor(1).0;
        }
    }

    /// Przyjęcie wczytanych kafli i wysłanie na GPU (z limitem na klatkę, bez zacięć).
    pub fn receive(&mut self, device: &wgpu::Device, queue: &wgpu::Queue) -> bool {
        let mut any = false;
        let mut budget = 40;
        let mut reseam = Vec::new();
        while budget > 0 {
            let Ok(l) = self.rx.try_recv() else { break };
            budget -= 1;
            any = true;
            self.queue.0.lock().unwrap().inflight.remove(&l.key);
            // złożone z dysku ortofoto zajmuje miejsce zwykłego
            let l = match l.key.0 {
                Kind::OrthoLocal => match l.payload {
                    Payload::Missing => {
                        self.local_miss.insert(l.key.1, self.frame);
                        continue;
                    }
                    // prawdziwy kafel zdążył dojść – nie zastępujemy go złożonym
                    _ if self.img.contains_key(&(Kind::Ortho, l.key.1)) => continue,
                    p => Loaded { key: Key(Kind::Ortho, l.key.1), payload: p },
                },
                _ => l,
            };
            match l.payload {
                Payload::Missing => {
                    self.missing.insert(l.key);
                }
                Payload::Dem { heights, grad, min, max, seams } => {
                    let t = l.key.1;
                    let hb: Vec<u8> = bytemuck::cast_slice(&heights).to_vec();
                    let gb: Vec<u8> = bytemuck::cast_slice(&grad).to_vec();
                    let ht = make_texture(device, queue, wgpu::TextureFormat::R32Float, DEM_RES, DEM_RES, &[hb], 4);
                    let mips = grad_mips(&grad);
                    let mut levels = vec![gb];
                    levels.extend(mips);
                    let gt = make_texture(device, queue, wgpu::TextureFormat::Rg16Float, DEM_RES, DEM_RES, &levels, 4);
                    let bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
                        label: Some("dem"),
                        layout: &self.dem_layout,
                        entries: &[
                            wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(&ht.create_view(&Default::default())) },
                            wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(&gt.create_view(&Default::default())) },
                        ],
                    });
                    self.dem.insert(t, GpuDem { bind, min, max, seams, used: self.frame });
                    // sąsiedzi z nieuzgodnionym brzegiem po stronie tego kafla – do przeliczenia
                    for (side, (dx, dy)) in [(1, (-1i64, 0i64)), (0, (1, 0)), (3, (0, -1)), (2, (0, 1))] {
                        let (nx, ny) = (t.x as i64 + dx, t.y as i64 + dy);
                        if nx < 0 || ny < 0 || nx >= 1 << t.z || ny >= 1 << t.z {
                            continue;
                        }
                        let nt = TileId::new(t.z, nx as u32, ny as u32);
                        if self.dem.get(&nt).is_some_and(|d| !d.seams[side]) {
                            reseam.push(nt);
                        }
                    }
                }
                Payload::Img(levels) => {
                    let size = (levels[0].len() / 4) as f32;
                    let s = size.sqrt() as u32;
                    let tex = make_texture(device, queue, wgpu::TextureFormat::Rgba8UnormSrgb, s, s, &levels, 4);
                    let bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
                        label: Some("img"),
                        layout: &self.img_layout,
                        entries: &[wgpu::BindGroupEntry {
                            binding: 0,
                            resource: wgpu::BindingResource::TextureView(&tex.create_view(&Default::default())),
                        }],
                    });
                    self.img.insert((l.key.0, l.key.1), GpuImg { bind, used: self.frame });
                }
            }
        }
        if !reseam.is_empty() {
            let (m, cv) = &*self.queue;
            let mut q = m.lock().unwrap();
            for t in reseam {
                let key = Key(Kind::Dem, t);
                if !q.inflight.contains(&key) {
                    q.list.push(Request { key, prio: -1e9, sticky: true });
                }
            }
            cv.notify_all();
        }
        any
    }

    pub fn dem_bind(&self, t: TileId) -> &wgpu::BindGroup {
        &self.dem[&t].bind
    }

    pub fn img_bind(&self, t: Option<(Kind, TileId)>) -> &wgpu::BindGroup {
        t.and_then(|t| self.img.get(&t)).map(|g| &g.bind).unwrap_or(&self.empty_img)
    }

    /// Wybór kafli do narysowania i zlecenia wczytania brakujących.
    pub fn select(&mut self, view: &View, s: &TileSettings) -> Vec<DrawTile> {
        self.frame += 1;
        self.overlay_draws.clear();
        self.layer_drawn.clear();
        // usługi bywają chwilowo niedostępne: nieudane kafle warstw – próba od nowa co jakiś czas
        if self.frame % 3600 == 0 {
            self.missing.retain(|k| !matches!(k.0, Kind::Overlay(_)));
        }
        let frame = view.frame;
        let planes = frustum(view);
        let focal = view.size[1] as f64 * 0.5 / (45f64.to_radians() / 2.0).tan();
        let mut out = Vec::new();
        let mut want: Vec<Request> = Vec::new();
        // korzenie: kafle z5 wokół kamery
        let rz = 5u8;
        let n = 1i64 << rz;
        let (cx, cy) = (
            (view.eye.x / frame.m + frame.ox) * n as f64,
            (view.eye.z / frame.m + frame.oy) * n as f64,
        );
        let span = (s.max_dist / (frame.m / n as f64)).ceil() as i64 + 1;
        let mut stack: Vec<TileId> = Vec::new();
        for y in (cy as i64 - span).max(0)..=(cy as i64 + span).min(n - 1) {
            for x in cx as i64 - span..=cx as i64 + span {
                stack.push(TileId::new(rz, x.rem_euclid(n) as u32, y as u32));
            }
        }
        stack.sort_by_key(|t| (t.x, t.y));
        stack.dedup();
        let wrap_x = |t: &TileId| -> f64 {
            // kafel najbliżej kamery w poziomie (przejście przez południk 180°)
            let n = (1u64 << t.z) as f64;
            let mut wx = t.x as f64 / n;
            let ex = view.eye.x / frame.m + frame.ox;
            if wx - ex > 0.5 {
                wx -= 1.0;
            } else if ex - wx > 0.5 {
                wx += 1.0;
            }
            wx
        };
        while let Some(t) = stack.pop() {
            let nn = (1u64 << t.z) as f64;
            let wx = wrap_x(&t);
            let wy = t.y as f64 / nn;
            let (x0, z0) = frame.world_to_local(wx, wy);
            let size = frame.m / nn;
            let (mut lo, mut hi) = self.dem_minmax(t).unwrap_or((-50.0, 3000.0));
            if !lo.is_finite() || !hi.is_finite() {
                lo = -50.0;
                hi = 3000.0;
            }
            let ex = view.exag;
            // najbliższy punkt prostopadłościanu
            let px = view.eye.x.clamp(x0, x0 + size);
            let pz = view.eye.z.clamp(z0, z0 + size);
            let hd = (px - view.eye.x).hypot(pz - view.eye.z);
            if hd > s.max_dist {
                continue;
            }
            let drop = hd * hd / (2.0 * EARTH_R) * 0.87;
            let y0 = lo as f64 * ex - drop - 50.0;
            let y1 = hi as f64 * ex - drop + 50.0;
            let py = view.eye.y.clamp(y0, y1);
            let d = DVec3::new(px, py, pz).distance(view.eye).max(1.0);
            let rel_min = DVec3::new(x0, y0, z0) - view.eye;
            let rel_max = DVec3::new(x0 + size, y1, z0 + size) - view.eye;
            if !aabb_visible(&planes, rel_min, rel_max) {
                continue;
            }
            let px_size = size / d * focal;
            if t.z < MAX_RENDER_Z && (px_size > s.target_px as f64 || t.z < 6) {
                let (x, y, z) = (t.x * 2, t.y * 2, t.z + 1);
                for (dx, dy) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
                    stack.push(TileId::new(z, x + dx, y + dy));
                }
                continue;
            }
            // dane: najlepszy rezydentny przodek; własny kafel do wczytania
            let dem_z = t.z.min(download::TERRAIN_MAX_Z);
            let own = ancestor_at(t, dem_z);
            let prio = d as f32 / 1000.0;
            if !self.dem.contains_key(&own) && !self.missing.contains(&Key(Kind::Dem, own)) {
                want.push(Request { key: Key(Kind::Dem, own), prio, sticky: false });
                // przodkowie też (szybko wypełniają obraz)
                for dz in [2u8, 4, 6] {
                    if own.z > 4 + dz {
                        let a = own.ancestor(dz).0;
                        if !self.dem.contains_key(&a) && !self.missing.contains(&Key(Kind::Dem, a)) {
                            want.push(Request { key: Key(Kind::Dem, a), prio: prio - 1000.0 + a.z as f32, sticky: false });
                        }
                    }
                }
            }
            let Some((dem_t, dem_x)) = self.find(&t, Kind::Dem) else {
                continue;
            };
            let mut img = None;
            if s.imagery {
                let iz = t.z.min(SAT_MAX_Z);
                let own_i = ancestor_at(t, iz);
                if !self.img.contains_key(&(Kind::Sat, own_i)) && !self.missing.contains(&Key(Kind::Sat, own_i)) {
                    want.push(Request { key: Key(Kind::Sat, own_i), prio: prio + 0.5, sticky: false });
                    if own_i.z > 8 {
                        let a = own_i.ancestor(3).0;
                        if !self.img.contains_key(&(Kind::Sat, a)) && !self.missing.contains(&Key(Kind::Sat, a)) {
                            want.push(Request { key: Key(Kind::Sat, a), prio: prio - 500.0, sticky: false });
                        }
                    }
                }
                // ortofoto krajowe na bliskich kaflach, Sentinel-2 w tle i dalej
                let mut ortho = None;
                if s.ortho && t.z >= ORTHO_LOCAL_MIN_Z {
                    let own_o = ancestor_at(t, t.z.min(national::ORTHO_MAX_Z));
                    let covered = *self.ortho_memo.entry(own_o).or_insert_with(|| national::has_ortho(own_o));
                    // promień pobierania: kilka kafli danego poziomu od kamery, najwyżej 12 km
                    // (dalej szczegóły ortofoto i tak nie są widoczne; już wczytane kafle zostają w użyciu)
                    let o_size = frame.m / (1u64 << own_o.z) as f64;
                    let near = own_o.z >= national::ORTHO_MIN_Z && d < (o_size * ORTHO_TILES_RADIUS).min(ORTHO_MAX_DIST);
                    if covered && !self.img.contains_key(&(Kind::Ortho, own_o)) {
                        if near && !self.missing.contains(&Key(Kind::Ortho, own_o)) {
                            want.push(Request { key: Key(Kind::Ortho, own_o), prio: prio + 0.3, sticky: false });
                        } else if self.local_miss.get(&own_o).map_or(true, |f|self.frame - f > ORTHO_LOCAL_RETRY) {
                            // dalej: bez sieci, tylko złożenie z kafli pobranych wcześniej (jeden obraz
                            // zamiast wielu drobnych – mniej tekstur i rysowania przy oddalaniu)
                            want.push(Request { key: Key(Kind::OrthoLocal, own_o), prio: prio + 0.6, sticky: false });
                        }
                    }
                    if covered {
                        ortho = self.find(&t, Kind::Ortho).filter(|(a, _)| a.z >= ORTHO_LOCAL_MIN_Z);
                    }
                }
                img = match ortho {
                    Some((a, x)) => Some(((Kind::Ortho, a), x)),
                    None => self.find(&t, Kind::Sat).map(|(a, x)| ((Kind::Sat, a), x)),
                };
            }
            for &(i, opacity) in &s.overlays {
                let min_z = layers::min_z(i);
                if t.z < min_z {
                    continue;
                }
                let kind = Kind::Overlay(i);
                let own = ancestor_at(t, t.z.min(layers::MAX_Z));
                if !self.img.contains_key(&(kind, own)) && !self.missing.contains(&Key(kind, own)) {
                    want.push(Request { key: Key(kind, own), prio: prio + 0.4, sticky: false });
                }
                if let Some((a, x)) = self.find(&t, kind).filter(|(a, _)| a.z >= min_z) {
                    if let Some(g) = self.img.get_mut(&(kind, a)) {
                        g.used = self.frame;
                    }
                    self.overlay_draws.push(OverlayDraw { tile: out.len(), image: (kind, a), xform: x, opacity });
                    *self.layer_drawn.entry(i).or_default() += 1;
                }
            }
            if let Some(d) = self.dem.get_mut(&dem_t) {
                d.used = self.frame;
            }
            if let Some((it, _)) = img {
                if let Some(g) = self.img.get_mut(&it) {
                    g.used = self.frame;
                }
            }
            out.push(DrawTile {
                tile: t,
                dem: dem_t,
                dem_xform: dem_x,
                img,
                rect: [x0 as f32, z0 as f32, size as f32, (size * 0.02).max(10.0) as f32],
            });
        }
        // zlecenia: najbliższe pierwsze; reszta kolejki zastąpiona
        {
            let (m, cv) = &*self.queue;
            let mut q = m.lock().unwrap();
            want.sort_by(|a, b| a.prio.total_cmp(&b.prio));
            want.dedup_by_key(|r| r.key);
            let keep: Vec<Request> = q.list.drain(..).filter(|r| r.sticky).collect();
            q.list = keep;
            let inflight = q.inflight.clone();
            q.list.extend(want.into_iter().filter(|r| !inflight.contains(&r.key)).take(256));
            self.loading = q.list.len() + q.inflight.len();
            self.loading_kinds = [0; 4];
            self.layer_loading.clear();
            for k in q.list.iter().map(|r| r.key.0).chain(q.inflight.iter().map(|k| k.0)) {
                self.loading_kinds[match k {
                    Kind::Dem => 0,
                    Kind::Sat => 1,
                    Kind::Ortho => 2,
                    Kind::OrthoLocal => 3,
                    Kind::Overlay(i) => {
                        *self.layer_loading.entry(i).or_default() += 1;
                        continue;
                    }
                }] += 1;
            }
            cv.notify_all();
        }
        self.evict(s);
        out
    }

    /// Najbliższy rezydentny przodek (albo sam kafel) i przekształcenie współrzędnych.
    fn find(&self, t: &TileId, kind: Kind) -> Option<(TileId, [f32; 3])> {
        let start = match kind {
            Kind::Dem => t.z.min(download::TERRAIN_MAX_Z),
            Kind::Sat => t.z.min(SAT_MAX_Z),
            Kind::Ortho | Kind::OrthoLocal => t.z.min(national::ORTHO_MAX_Z),
            Kind::Overlay(_) => t.z.min(layers::MAX_Z),
        };
        for z in (0..=start).rev() {
            let dz = t.z - z;
            let (a, off, k) = t.ancestor(dz);
            let ok = match kind {
                Kind::Dem => self.dem.contains_key(&a),
                Kind::Sat | Kind::Ortho | Kind::OrthoLocal | Kind::Overlay(_) => self.img.contains_key(&(kind, a)),
            };
            if ok {
                return Some((a, [off[0], off[1], k]));
            }
        }
        None
    }

    fn evict(&mut self, s: &TileSettings) {
        if self.dem.len() > s.max_dem {
            let mut ages: Vec<u64> = self.dem.values().map(|d| d.used).collect();
            ages.sort_unstable();
            let cut = ages[self.dem.len() - s.max_dem * 4 / 5];
            // kafle z ≤ 6 zostają (tło całego widoku)
            self.dem.retain(|t, d| d.used >= cut || t.z <= 6);
        }
        if self.img.len() > s.max_img {
            let mut ages: Vec<u64> = self.img.values().map(|d| d.used).collect();
            ages.sort_unstable();
            let cut = ages[self.img.len() - s.max_img * 4 / 5];
            // kafle zmienne (pogoda, kartogram) nie mają przywileju tła: inaczej każda godzina zostawałaby w pamięci
            self.img.retain(|k, d| d.used >= cut || (k.1.z <= 6 && !matches!(k.0, Kind::Overlay(i) if i >= crate::weather::FIELD_BASE)));
        }
    }

    /// Wysokość terenu w punkcie ramki z danych, które widać (do wybierania punktów i etykiet).
    pub fn height_at(&self, lon: f64, lat: f64) -> Option<f64> {
        self.heights.sample_cached(lon, lat).map(|h| h as f64)
    }
}

fn ancestor_at(t: TileId, z: u8) -> TileId {
    if t.z <= z {
        t
    } else {
        t.ancestor(t.z - z).0
    }
}

/// Wolne usługi: ortofoto, warstwy WMS i teren z WCS GUGiK.
fn is_slow(k: Key) -> bool {
    match k.0 {
        Kind::Ortho | Kind::Overlay(_) => true,
        Kind::Dem => download::dem_from_wcs(k.1),
        _ => false,
    }
}

fn worker(q: Arc<(Mutex<Queue>, Condvar)>, tx: Sender<Loaded>, heights: Heights, ctx: eframe::egui::Context) {
    loop {
        let req = {
            let (m, cv) = &*q;
            let mut g = m.lock().unwrap();
            loop {
                // najważniejsze: najmniejszy priorytet (ortofoto tylko, gdy są na nie wolne wątki)
                let slow_free = g.slow < SLOW_WORKERS;
                let best = g
                    .list
                    .iter()
                    .enumerate()
                    .filter(|(_, r)| slow_free || !is_slow(r.key))
                    .min_by(|a, b| a.1.prio.total_cmp(&b.1.prio))
                    .map(|(i, _)| i);
                if let Some(i) = best {
                    let r = g.list.swap_remove(i);
                    if g.inflight.insert(r.key) {
                        if is_slow(r.key) {
                            g.slow += 1;
                        }
                        break r;
                    }
                    continue;
                }
                g = cv.wait(g).unwrap();
            }
        };
        let payload = match req.key.0 {
            Kind::Dem => match heights.load(req.key.1) {
                Some(d) => prepare_dem(&heights, req.key.1, &d),
                None => Payload::Missing,
            },
            Kind::Sat => load_img(&heights.store(), req.key.1, Layer::Sat, 256),
            Kind::Ortho => load_img(&heights.store(), req.key.1, Layer::Ortho, national::ORTHO_PX),
            Kind::Overlay(i) => match layers::load(heights.store().root(), i, req.key.1) {
                Some(img) => img_levels(img, layers::TILE_PX),
                None => Payload::Missing,
            },
            Kind::OrthoLocal => match compose_ortho(&heights.store(), req.key.1, ORTHO_LOCAL_DEPTH) {
                Some(img) => img_levels(img, national::ORTHO_PX),
                None => Payload::Missing,
            },
        };
        if is_slow(req.key) {
            let (m, cv) = &*q;
            m.lock().unwrap().slow -= 1;
            cv.notify_all();
        }
        if tx.send(Loaded { key: req.key, payload }).is_err() {
            break;
        }
        ctx.request_repaint();
    }
}

fn load_img(store: &Store, t: TileId, layer: Layer, size: u32) -> Payload {
    let Ok(blob) = download::get(store, layer, t) else {
        return Payload::Missing;
    };
    let Some(jpg) = bake::jpeg(&blob) else {
        return Payload::Missing;
    };
    let Ok(img) = image::load_from_memory_with_format(jpg, image::ImageFormat::Jpeg) else {
        return Payload::Missing;
    };
    img_levels(img.to_rgba8(), size)
}

/// Zapisany na dysku kafel ortofoto (bez pobierania).
fn stored_ortho(store: &Store, t: TileId) -> Option<image::RgbaImage> {
    let blob = store.read(Layer::Ortho, t)?;
    let jpg = bake::jpeg(&blob)?;
    Some(image::load_from_memory_with_format(jpg, image::ImageFormat::Jpeg).ok()?.to_rgba8())
}

/// Ortofoto kafla złożone z zapisanych kafli niższych poziomów (do `depth` w dół);
/// brakujące ćwiartki z tła satelitarnego. `None`, gdy na dysku nie ma żadnej części.
fn compose_ortho(store: &Store, t: TileId, depth: u8) -> Option<image::RgbaImage> {
    if let Some(img) = stored_ortho(store, t) {
        return Some(img);
    }
    if depth == 0 || t.z >= national::ORTHO_MAX_Z {
        return None;
    }
    let n = national::ORTHO_PX;
    let h = n / 2;
    let mut parts = Vec::new();
    for (dx, dy) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
        let c = TileId::new(t.z + 1, t.x * 2 + dx, t.y * 2 + dy);
        if let Some(img) = compose_ortho(store, c, depth - 1) {
            parts.push((dx, dy, img));
        }
    }
    if parts.is_empty() {
        return None;
    }
    let mut canvas = match crate::download::sat_background(store, t) {
        Some(b) => image::imageops::resize(&b, n, n, image::imageops::FilterType::Triangle),
        None => image::RgbaImage::from_pixel(n, n, image::Rgba([96, 104, 88, 255])),
    };
    for (dx, dy, img) in parts {
        let q = image::imageops::resize(&img, h, h, image::imageops::FilterType::Triangle);
        image::imageops::replace(&mut canvas, &q, (dx * h) as i64, (dy * h) as i64);
    }
    Some(canvas)
}

/// Obraz kafla z pełnym łańcuchem mipmap.
fn img_levels(img: image::RgbaImage, size: u32) -> Payload {
    let img = if img.width() != size || img.height() != size {
        image::imageops::resize(&img, size, size, image::imageops::FilterType::Triangle)
    } else {
        img
    };
    let mut levels = vec![img.into_raw()];
    let mut s = size as usize;
    while s > 1 {
        let prev = levels.last().unwrap();
        let h = s / 2;
        let mut next = vec![0u8; h * h * 4];
        for y in 0..h {
            for x in 0..h {
                for c in 0..4 {
                    let i = |xx: usize, yy: usize| prev[(yy * s + xx) * 4 + c] as u32;
                    next[(y * h + x) * 4 + c] =
                        ((i(2 * x, 2 * y) + i(2 * x + 1, 2 * y) + i(2 * x, 2 * y + 1) + i(2 * x + 1, 2 * y + 1) + 2) / 4) as u8;
                }
            }
        }
        levels.push(next);
        s = h;
    }
    Payload::Img(levels)
}

/// Siatka narożników 257×257 z pikseli kafla (z sąsiadami, jeśli są w pamięci) i gradient.
fn prepare_dem(heights: &Heights, t: TileId, d: &Dem) -> Payload {
    let n = 1u32 << t.z;
    let px = |dem: &Dem, x: usize, y: usize| -> f32 {
        if dem.w == 256 && dem.h == 256 {
            dem.elev[y * 256 + x]
        } else {
            dem.elev[(y * dem.h / 256).min(dem.h - 1) * dem.w + (x * dem.w / 256).min(dem.w - 1)]
        }
    };
    let nb = |dx: i64, dy: i64| -> Option<Arc<Dem>> {
        let (x, y) = (t.x as i64 + dx, t.y as i64 + dy);
        if y < 0 || y >= n as i64 {
            return None;
        }
        heights.cached(TileId::new(t.z, x.rem_euclid(n as i64) as u32, y as u32))
    };
    let (l, r, u, b) = (nb(-1, 0), nb(1, 0), nb(0, -1), nb(0, 1));
    let seams = [l.is_some(), r.is_some(), u.is_some(), b.is_some()];
    // piksel o współrzędnych -1..=256 (poza kaflem – z sąsiada albo przycięty)
    let get = |x: i32, y: i32| -> f32 {
        let (cx, cy) = (x.clamp(0, 255) as usize, y.clamp(0, 255) as usize);
        if x < 0 {
            if let Some(n) = &l {
                return px(n, 255, cy);
            }
        } else if x > 255 {
            if let Some(n) = &r {
                return px(n, 0, cy);
            }
        }
        if y < 0 {
            if let Some(n) = &u {
                return px(n, cx, 255);
            }
        } else if y > 255 {
            if let Some(n) = &b {
                return px(n, cx, 0);
            }
        }
        px(d, cx, cy)
    };
    let res = DEM_RES as usize;
    let mut hs = vec![0f32; res * res];
    let (mut min, mut max) = (f32::INFINITY, f32::NEG_INFINITY);
    for j in 0..res {
        for i in 0..res {
            let (x, y) = (i as i32, j as i32);
            let v = (get(x - 1, y - 1) + get(x, y - 1) + get(x - 1, y) + get(x, y)) * 0.25;
            hs[j * res + i] = v;
            min = min.min(v);
            max = max.max(v);
        }
    }
    let lat = t.bbox().center()[1];
    let spacing = meters_per_pixel(lat, t.z) as f32;
    let mut grad = vec![half::f16::ZERO; res * res * 2];
    for j in 0..res {
        for i in 0..res {
            let (i0, i1) = (i.saturating_sub(1), (i + 1).min(res - 1));
            let (j0, j1) = (j.saturating_sub(1), (j + 1).min(res - 1));
            let gx = (hs[j * res + i1] - hs[j * res + i0]) / ((i1 - i0) as f32 * spacing);
            let gz = (hs[j1 * res + i] - hs[j0 * res + i]) / ((j1 - j0) as f32 * spacing);
            grad[(j * res + i) * 2] = half::f16::from_f32(gx);
            grad[(j * res + i) * 2 + 1] = half::f16::from_f32(gz);
        }
    }
    Payload::Dem { heights: hs, grad, min, max, seams }
}

/// Mipmapy gradientu (średnie 2×2; rozmiar 257 → 128 → …).
fn grad_mips(g: &[half::f16]) -> Vec<Vec<u8>> {
    let mut out = Vec::new();
    let mut cur: Vec<f32> = g.iter().map(|v| v.to_f32()).collect();
    let mut s = DEM_RES as usize;
    while s > 1 {
        let h = (s / 2).max(1);
        let mut next = vec![0f32; h * h * 2];
        for y in 0..h {
            for x in 0..h {
                for c in 0..2 {
                    let at = |xx: usize, yy: usize| cur[((yy.min(s - 1)) * s + xx.min(s - 1)) * 2 + c];
                    next[(y * h + x) * 2 + c] = (at(2 * x, 2 * y) + at(2 * x + 1, 2 * y) + at(2 * x, 2 * y + 1) + at(2 * x + 1, 2 * y + 1)) * 0.25;
                }
            }
        }
        let bytes: Vec<half::f16> = next.iter().map(|v| half::f16::from_f32(*v)).collect();
        out.push(bytemuck::cast_slice(&bytes).to_vec());
        cur = next;
        s = h;
    }
    out
}

/// Tekstura z poziomami mipmap (`levels[0]` – pełny rozmiar).
pub fn make_texture(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    format: wgpu::TextureFormat,
    w: u32,
    h: u32,
    levels: &[Vec<u8>],
    bpp: u32,
) -> wgpu::Texture {
    let tex = device.create_texture(&wgpu::TextureDescriptor {
        label: None,
        size: wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
        mip_level_count: levels.len() as u32,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    let (mut lw, mut lh) = (w, h);
    for (i, data) in levels.iter().enumerate() {
        queue.write_texture(
            wgpu::TexelCopyTextureInfo { texture: &tex, mip_level: i as u32, origin: wgpu::Origin3d::ZERO, aspect: wgpu::TextureAspect::All },
            data,
            wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(lw * bpp), rows_per_image: Some(lh) },
            wgpu::Extent3d { width: lw, height: lh, depth_or_array_layers: 1 },
        );
        lw = (lw / 2).max(1);
        lh = (lh / 2).max(1);
    }
    tex
}

type Planes = [Vec4; 6];

fn frustum(view: &View) -> Planes {
    let m = view.view_proj_rel();
    let r = [m.row(0), m.row(1), m.row(2), m.row(3)];
    // odwrócona głębia, nieskończona: bliska płaszczyzna z = w, daleka pominięta
    let mut p = [r[3] + r[0], r[3] - r[0], r[3] + r[1], r[3] - r[1], r[3] - r[2], r[2]];
    for v in &mut p {
        let l = v.truncate().length();
        if l > 0.0 {
            *v /= l;
        }
    }
    p[5] = Vec4::new(0.0, 0.0, 0.0, 1.0);
    p
}

fn aabb_visible(planes: &Planes, lo: DVec3, hi: DVec3) -> bool {
    for p in planes.iter().take(5) {
        let v = DVec3::new(
            if p.x >= 0.0 { hi.x } else { lo.x },
            if p.y >= 0.0 { hi.y } else { lo.y },
            if p.z >= 0.0 { hi.z } else { lo.z },
        );
        if p.x as f64 * v.x + p.y as f64 * v.y + p.z as f64 * v.z + p.w as f64 * 1.0 < 0.0 {
            return false;
        }
    }
    true
}
