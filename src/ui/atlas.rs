//! Atlas 3D: kamera nad terenem Polski, narzędzia (eksploracja, pomiar), soczewki, analizy w tle,
//! etykiety i znaczniki. Panele boczne – w `panels.rs`.

use std::sync::mpsc::{channel, Receiver};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use eframe::egui::{self, Align2, Color32, FontId, Pos2, Rect, Sense, Stroke, Vec2};
use eframe::egui_wgpu;
use glam::DVec3;
use serde::{Deserialize, Serialize};

use crate::analysis;
use crate::fmt;
use crate::geo::{bearing, haversine, lat_to_y, lon_to_x, world_m, BBox};
use crate::heights::{Grid, Heights};
use crate::metrics::{self, TimeModel};
use crate::osm::{PoiKind, SectorId};
use crate::render::camera::{Camera, Frame, View};
use crate::render::{LineInst, Renderer, Scene};
use crate::sun::{self, Day};
use crate::ui::theme;
use crate::world::{Snapshot, World};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Tool {
    Explore,
    Measure,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Lens {
    None,
    Band,
    Rel,
    Vis,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Style {
    Terrain,
    Hypso,
    Slope,
    Aspect,
    Satellite,
    Map,
}

impl Style {
    pub const ALL: [Style; 6] = [Style::Terrain, Style::Satellite, Style::Map, Style::Hypso, Style::Slope, Style::Aspect];

    pub fn label(self) -> &'static str {
        match self {
            Style::Terrain => "Teren",
            Style::Hypso => "Hipsometria",
            Style::Slope => "Nachylenie (lawiny)",
            Style::Aspect => "Ekspozycja",
            Style::Satellite => "Satelita",
            Style::Map => "Mapa",
        }
    }

    fn from_code(c: u32) -> Option<Style> {
        Style::ALL.into_iter().find(|s| s.code() == c)
    }

    fn code(self) -> u32 {
        match self {
            Style::Terrain => 0,
            Style::Hypso => 1,
            Style::Slope => 2,
            Style::Aspect => 3,
            Style::Satellite => 4,
            Style::Map => 5,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Quality {
    Min,
    Low,
    Mid,
    High,
    Ultra,
    Custom,
}

impl Quality {
    pub const PRESETS: [Quality; 5] = [Quality::Min, Quality::Low, Quality::Mid, Quality::High, Quality::Ultra];

    pub fn label(self) -> &'static str {
        match self {
            Quality::Min => "Minimalna",
            Quality::Low => "Niska",
            Quality::Mid => "Średnia",
            Quality::High => "Wysoka",
            Quality::Ultra => "Ultra",
            Quality::Custom => "Własna",
        }
    }

    /// (skala renderu, rozmiar kafla px, kafle DEM, kafle zdjęć)
    pub fn values(self) -> (f32, f32, usize, usize) {
        match self {
            Quality::Min => (0.5, 560.0, 200, 260),
            Quality::Low => (0.7, 460.0, 280, 360),
            Quality::Mid => (0.85, 380.0, 380, 480),
            Quality::High | Quality::Custom => (1.0, 300.0, 520, 640),
            Quality::Ultra => (1.0, 220.0, 900, 1100),
        }
    }
}

/// Warstwa tematyczna w panelu: kolejność listy to kolejność rysowania (od spodu).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LayerState {
    pub id: String,
    pub on: bool,
    pub opacity: f32,
}

/// Ustawienia zapamiętywane między uruchomieniami.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Prefs {
    pub style: Style,
    pub exag: f64,
    pub trails: bool,
    pub labels: bool,
    pub contours: bool,
    pub km_grid: bool,
    pub shadows: bool,
    pub snow: bool,
    /// Ortofotomapa GUGiK w stylu satelitarnym.
    pub ortho: bool,
    /// Maska pokrycia terenu z OSM (wody, lasy, skały) w stylu „teren”.
    pub landcover: bool,
    /// Lekka mapa normalnych: drobna rzeźba z bliska.
    pub detail: bool,
    /// Pora dnia i data: `None` – teraz.
    pub custom_time: Option<(i32, u32, u32, f64)>,
    pub quality: Quality,
    pub render_scale: f32,
    pub detail_px: f32,
    pub max_dem: usize,
    pub max_img: usize,
    pub label_density: f32,
    pub din: bool,
    pub pace: f64,
    pub band_tol: f32,
    pub rel_range: f32,
    pub vs_eye: f32,
    pub camera: (f64, f64, f64, f64, f64, f64),
    pub panel_open: bool,
    /// Drzewa i budynki z lidaru (NMPT) na bliskich kaflach.
    pub surface: bool,
    pub layers: Vec<LayerState>,
    /// Włączone dane na żywo.
    pub feeds: Vec<String>,
}

impl Default for Prefs {
    fn default() -> Self {
        let c = Camera::default();
        let (s, d, md, mi) = Quality::High.values();
        Prefs {
            style: Style::Satellite,
            exag: 1.0,
            trails: true,
            labels: true,
            contours: false,
            km_grid: false,
            shadows: true,
            snow: true,
            ortho: true,
            landcover: true,
            detail: true,
            custom_time: None,
            quality: Quality::High,
            render_scale: s,
            detail_px: d,
            max_dem: md,
            max_img: mi,
            label_density: 1.0,
            din: false,
            pace: 1.0,
            band_tol: 20.0,
            rel_range: 400.0,
            vs_eye: 1.7,
            camera: (c.lon, c.lat, c.height, c.heading, c.pitch, c.distance),
            panel_open: false,
            surface: false,
            layers: Vec::new(),
            feeds: Vec::new(),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum Selection {
    None,
    Poi(usize),
    Point { lon: f64, lat: f64 },
    Trail(u32),
    /// Punkt danych na żywo: (feed, klucz).
    Feed(String, String),
}

/// Wynik analizy widoczności z punktu.
pub struct VsInfo {
    pub lon: f64,
    pub lat: f64,
    pub area_km2: f64,
    pub observer: f32,
    /// Widoczne punkty (indeks w migawce, odległość m).
    pub visible: Vec<(usize, f64)>,
    pub eye: f32,
}

/// Wynik statystyk wysokości względnej.
pub struct RelInfo {
    pub above: f64,
    pub below: f64,
}

struct FlyTo {
    from: Camera,
    to: Camera,
    t0: Instant,
    dur: f32,
}

enum JobResult {
    Viewshed(u64, Box<(VsInfo, Vec<u8>, u32, u32, [f64; 4])>),
    LiveVs(u64, Box<(Vec<u8>, u32, u32, [f64; 4])>),
    Shadow(u64, Box<(Vec<u8>, u32, u32, [f64; 4])>),
    Rel(u64, RelInfo),
    Los(u64, Option<f64>, f64),
    Land(Option<Box<(Vec<u8>, u32, [f64; 4], Vec<f32>, u32)>>),
}

/// Prośby atlasu do aplikacji.
#[derive(Default)]
pub struct Requests {
    pub open_settings: bool,
    pub check_updates: bool,
}

pub struct Atlas {
    pub prefs: Prefs,
    pub cam: Camera,
    pub frame: Frame,
    frame_version: u64,
    pub heights: Heights,
    pub world: World,
    pub renderer: Option<Renderer>,
    pub snap: Arc<Snapshot>,
    pub tool: Tool,
    pub lens: Lens,
    pub lens_pin: Option<(f64, f64, f32)>,
    pub measure: Vec<(f64, f64, f32)>,
    pub los: Option<(Option<f64>, f64)>,
    pub selection: Selection,
    pub hover: Option<(f64, f64, f32)>,
    pub vs: Option<VsInfo>,
    pub vs_busy: bool,
    pub rel: Option<RelInfo>,
    live_vs: Option<(f64, f64)>,
    live_vs_busy: bool,
    live_vs_at: Instant,
    shadow_key: Option<(i64, i64, i64, i64)>,
    shadow_busy: bool,
    shadow_rect: Option<[f64; 4]>,
    vs_rect: Option<[f64; 4]>,
    live_rect: Option<[f64; 4]>,
    land_key: Option<(SectorId, u64)>,
    land_busy: bool,
    land_rect: Option<[f64; 4]>,
    gen: u64,
    tx: std::sync::mpsc::Sender<JobResult>,
    rx: Receiver<JobResult>,
    fly: Option<FlyTo>,
    pub panorama: Option<Camera>,
    drag_grab: Option<DVec3>,
    /// Obrót: chwycony punkt terenu (dł., szer., wysokość) i jego piksel w widoku.
    orbit: Option<(f64, f64, f32, [f32; 2])>,
    pub ctx_menu: Option<(Pos2, f64, f64, f32)>,
    pub search: Option<crate::ui::search::Search>,
    pub help: bool,
    pub layers_open: bool,
    pub weather_open: bool,
    pub weather_field: Option<crate::weather::Field>,
    pub weather_hour: Option<usize>,
    pub weather_play: bool,
    weather_step: Instant,
    pub satellite: bool,
    pub wind_particles: bool,
    particles: Vec<(f64, f64, f32)>,
    pub stats_open: bool,
    pub stats_bars: bool,
    pub stats_level: crate::stats::Level,
    pub stats_year: i32,
    pub stats_method: crate::stats::Method,
    pub stats_query: String,
    pub stats_var: Option<crate::stats::Variable>,
    pub stats: crate::stats::Shared,
    pub feeds: crate::feeds::Hub,
    /// Punkty danych na żywo na ekranie w poprzedniej klatce.
    feed_hits: Vec<(Pos2, f32, &'static str, String)>,
    /// Zapytanie o kliknięty punkt: (dł., szer., warstwy) i zbierane wyniki.
    pub point_query: Option<((f64, f64, Vec<u16>), Arc<std::sync::Mutex<crate::query::PointInfo>>)>,
    pub layer_query: String,
    pub user_presets: Vec<crate::layers::Preset>,
    pub new_preset: String,
    /// Legendy: pobrane w tle obrazy i gotowe tekstury.
    pub legend_inbox: Arc<std::sync::Mutex<Vec<(u16, Option<image::RgbaImage>)>>>,
    pub legends: std::collections::HashMap<u16, Option<egui::TextureHandle>>,
    pub legend_asked: std::collections::HashSet<u16>,
    pub tab: crate::ui::panels::Tab,
    pub toast: Option<(String, Instant)>,
    pub last_view: Option<View>,
    fps: (Instant, u32, f32),
    screenshot: Option<std::path::PathBuf>,
    pub requests: Requests,
    last_want: Instant,
    pub status_line: String,
}

/// Kolory linii (liniowe, do bufora sRGB).
fn lin(c: [u8; 3], a: f32) -> [f32; 4] {
    let f = |v: u8| (v as f32 / 255.0).powf(2.2);
    [f(c[0]), f(c[1]), f(c[2]), a]
}

impl Atlas {
    pub fn new(heights: Heights, world: World, prefs: Option<Prefs>) -> Self {
        let mut prefs = prefs.unwrap_or_default();
        crate::download::set_surface(prefs.surface);
        // katalog mógł się zmienić: nieznane warstwy wypadają, nowe dochodzą wyłączone
        let cat = crate::layers::catalog();
        prefs.layers.retain(|l| crate::layers::index_of(&l.id).is_some());
        for d in &cat.layers {
            if !prefs.layers.iter().any(|l| l.id == d.id) {
                prefs.layers.push(LayerState { id: d.id.clone(), on: false, opacity: d.opacity });
            }
        }
        let (lon, lat, height, heading, pitch, distance) = prefs.camera;
        let cam = Camera { lon, lat, height, heading, pitch, distance, ..Camera::default() };
        metrics::set_model(if prefs.din { TimeModel::Din } else { TimeModel::Pttk }, prefs.pace);
        let (tx, rx) = channel();
        Atlas {
            frame: Frame::at(lon, lat),
            frame_version: 1,
            prefs,
            cam,
            heights,
            snap: world.snapshot(),
            world,
            renderer: None,
            tool: Tool::Explore,
            lens: Lens::None,
            lens_pin: None,
            measure: Vec::new(),
            los: None,
            selection: Selection::None,
            hover: None,
            vs: None,
            vs_busy: false,
            rel: None,
            live_vs: None,
            live_vs_busy: false,
            live_vs_at: Instant::now(),
            shadow_key: None,
            shadow_busy: false,
            shadow_rect: None,
            vs_rect: None,
            live_rect: None,
            land_key: None,
            land_busy: false,
            land_rect: None,
            gen: 0,
            tx,
            rx,
            fly: None,
            panorama: None,
            drag_grab: None,
            orbit: None,
            ctx_menu: None,
            search: None,
            help: false,
            layers_open: false,
            weather_open: false,
            weather_field: None,
            weather_hour: None,
            weather_play: false,
            weather_step: Instant::now(),
            satellite: false,
            wind_particles: false,
            particles: Vec::new(),
            stats_open: false,
            stats_bars: false,
            stats_level: crate::stats::Level::County,
            stats_year: 2023,
            stats_method: crate::stats::Method::Quantiles,
            stats_query: String::new(),
            stats_var: None,
            stats: Default::default(),
            feeds: Default::default(),
            feed_hits: Vec::new(),
            point_query: None,
            layer_query: String::new(),
            user_presets: crate::layers::load_presets(),
            new_preset: String::new(),
            legend_inbox: Arc::default(),
            legends: Default::default(),
            legend_asked: Default::default(),
            tab: crate::ui::panels::Tab::Info,
            toast: None,
            last_view: None,
            fps: (Instant::now(), 0, 0.0),
            screenshot: None,
            requests: Requests::default(),
            last_want: Instant::now() - Duration::from_secs(10),
            status_line: String::new(),
        }
    }

    pub fn save_prefs(&mut self) -> Prefs {
        let c = self.panorama.unwrap_or(self.cam);
        self.prefs.camera = (c.lon, c.lat, c.height, c.heading, c.pitch, c.distance);
        self.prefs.clone()
    }

    /// Link do bieżącego widoku (kamera, styl): `psm://widok?…`.
    pub fn share_link(&self) -> String {
        let c = self.panorama.unwrap_or(self.cam);
        let mut s = format!(
            "psm://widok?lat={:.5}&lon={:.5}&az={:.0}&nach={:.0}&odl={:.0}&styl={}",
            c.lat,
            c.lon,
            c.heading,
            c.pitch,
            c.distance,
            self.prefs.style.code()
        );
        if self.prefs.exag != 1.0 {
            s.push_str(&format!("&prz={:.1}", self.prefs.exag));
        }
        s
    }

    /// Otwarcie linku `psm://widok?…`; `false`, gdy to nie jest link programu.
    pub fn apply_link(&mut self, link: &str) -> bool {
        let link = link.trim();
        let Some(q) = link.strip_prefix("psm://").and_then(|r| r.split_once('?')).map(|(_, q)| q) else {
            return false;
        };
        let mut cam = self.cam;
        let mut ok = false;
        for kv in q.split('&') {
            let Some((k, v)) = kv.split_once('=') else { continue };
            let v = v.replace("%3B", ";").replace("%2C", ",").replace("%3b", ";").replace("%2c", ",");
            let num = v.parse::<f64>().ok().filter(|x| x.is_finite());
            match (k, num) {
                ("lat", Some(x)) => {
                    cam.lat = x.clamp(-84.0, 84.0);
                    ok = true;
                }
                ("lon", Some(x)) => cam.lon = x,
                ("az", Some(x)) => cam.heading = x.rem_euclid(360.0),
                ("nach", Some(x)) => cam.pitch = x.clamp(4.0, 89.0),
                ("odl", Some(x)) => cam.distance = x.clamp(120.0, 900_000.0),
                ("styl", Some(x)) => {
                    if let Some(s) = Style::from_code(x as u32) {
                        self.prefs.style = s;
                    }
                }
                ("prz", Some(x)) => self.prefs.exag = x.clamp(1.0, 3.0),
                _ => {}
            }
        }
        if !ok {
            return false;
        }
        self.exit_panorama();
        cam.height = self.heights.sample_cached(cam.lon, cam.lat).map(|h| h as f64).unwrap_or(cam.height);
        self.start_fly(cam, 1.4);
        true
    }

    pub fn notify(&mut self, s: impl Into<String>) {
        self.toast = Some((s.into(), Instant::now()));
    }

    /// Dzień i godzina sceny.
    pub fn when(&self) -> (Day, f64) {
        match self.prefs.custom_time {
            Some((y, m, d, h)) => (Day { y, m, d }, h),
            None => {
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_secs_f64())
                    .unwrap_or(0.0);
                let off = sun::warsaw_offset_min(now) as f64 * 60.0;
                let local = now + off;
                (Day::today(), (local.rem_euclid(86_400.0)) / 3600.0)
            }
        }
    }

    pub fn sun(&self) -> (f64, f64) {
        let (day, hour) = self.when();
        sun::position(sun::warsaw_unix(day, hour), self.cam.lat, self.cam.lon)
    }

    // ------------------------------------------------------------------ kamera

    pub fn fly_to(&mut self, lon: f64, lat: f64, distance: Option<f64>) {
        let mut to = self.cam;
        to.lon = lon;
        to.lat = lat;
        to.height = self.heights.sample_cached(lon, lat).map(|h| h as f64).unwrap_or(self.cam.height);
        if let Some(d) = distance {
            to.distance = d;
        }
        self.start_fly(to, 1.2);
    }

    pub fn start_fly(&mut self, to: Camera, dur: f32) {
        self.fly = Some(FlyTo { from: self.cam, to, t0: Instant::now(), dur });
    }

    pub fn fit_bbox(&mut self, b: &BBox) {
        let c = b.center();
        let span = haversine(b.w, c[1], b.e, c[1]).max(haversine(c[0], b.s, c[0], b.n));
        self.fly_to(c[0], c[1], Some((span * 1.25).max(1500.0)));
    }

    pub fn enter_panorama(&mut self, lon: f64, lat: f64) {
        if self.panorama.is_none() {
            self.panorama = Some(self.cam);
        }
        let h = self.heights.sample_cached(lon, lat).unwrap_or(0.0) as f64;
        self.fly = None;
        self.cam.lon = lon;
        self.cam.lat = lat;
        self.cam.height = h + 1.7 / self.prefs.exag.max(1.0);
        self.cam.distance = 2.0;
        self.cam.pitch = -2.0;
        self.notify("Panorama: przeciągnij, żeby się rozejrzeć · Esc – powrót");
    }

    pub fn exit_panorama(&mut self) {
        if let Some(c) = self.panorama.take() {
            self.cam = c;
        }
    }

    fn animate(&mut self, ctx: &egui::Context) {
        if let Some(f) = &self.fly {
            let t = (f.t0.elapsed().as_secs_f32() / f.dur).min(1.0);
            let e = (t * t * (3.0 - 2.0 * t)) as f64;
            let (a, b) = (f.from, f.to);
            let mut c = b;
            c.lon = a.lon + (b.lon - a.lon) * e;
            c.lat = a.lat + (b.lat - a.lat) * e;
            c.height = a.height + (b.height - a.height) * e;
            // odległość: logarytmicznie, z lekkim „podskokiem” przy dalekich przelotach
            let hop = haversine(a.lon, a.lat, b.lon, b.lat);
            let lift = (hop * 0.6 - a.distance.max(b.distance)).max(0.0) * (std::f64::consts::PI * e).sin();
            c.distance = (a.distance.ln() + (b.distance.ln() - a.distance.ln()) * e).exp() + lift;
            let dh = ((b.heading - a.heading + 540.0) % 360.0) - 180.0;
            c.heading = (a.heading + dh * e).rem_euclid(360.0);
            c.pitch = a.pitch + (b.pitch - a.pitch) * e;
            self.cam = c;
            if t >= 1.0 {
                self.fly = None;
            }
            ctx.request_repaint();
        }
    }

    /// Nazwa najbliższego punktu (szczyt, schronisko…) w promieniu `r` m.
    pub fn name_near(&self, lon: f64, lat: f64, r: f64) -> Option<String> {
        self.snap
            .pois
            .iter()
            .filter(|p| p.poi.kind != PoiKind::Place)
            .map(|p| (haversine(lon, lat, p.poi.lon, p.poi.lat), p))
            .filter(|(d, _)| *d < r)
            .min_by(|a, b| a.0.total_cmp(&b.0))
            .map(|(_, p)| p.poi.name.clone())
    }

    // ------------------------------------------------------------------ analizy w tle

    fn grid_around(heights: &Heights, lon: f64, lat: f64, radius_m: f64, max_px: f64) -> Grid {
        let kx = 111_320.0 * lat.to_radians().cos();
        let bb = BBox::new(lon - radius_m / kx, lat - radius_m / 110_574.0, lon + radius_m / kx, lat + radius_m / 110_574.0);
        let mut z = 13u8;
        while z > 8 && 2.0 * radius_m / crate::geo::meters_per_pixel(lat, z) > max_px {
            z -= 1;
        }
        heights.grid(&bb, z)
    }

    fn world_rect(g: &Grid) -> [f64; 4] {
        let n = (1u64 << g.z) as f64 * 256.0;
        [g.px0 / n, g.py0 / n, (g.px0 + g.w as f64) / n, (g.py0 + g.h as f64) / n]
    }

    pub fn start_viewshed(&mut self, lon: f64, lat: f64) {
        self.gen += 1;
        let gen = self.gen;
        self.vs_busy = true;
        self.vs = None;
        let (heights, tx, snap, eye) = (self.heights.clone(), self.tx.clone(), self.snap.clone(), self.prefs.vs_eye);
        thread::spawn(move || {
            let g = Self::grid_around(&heights, lon, lat, 32_000.0, 2600.0);
            let (gx, gy) = g.to_grid(lon, lat);
            let v = analysis::viewshed(&g, gx, gy, eye, 0.0);
            let mut visible: Vec<(usize, f64)> = snap
                .pois
                .iter()
                .enumerate()
                .filter(|(_, p)| matches!(p.poi.kind, PoiKind::Peak | PoiKind::Hut | PoiKind::Shelter | PoiKind::Saddle))
                .filter_map(|(i, p)| {
                    let (px, py) = g.to_grid(p.poi.lon, p.poi.lat);
                    if !g.inside(px, py) {
                        return None;
                    }
                    let m = v.mask[py.round() as usize * g.w + px.round() as usize];
                    (m > 0).then(|| (i, haversine(lon, lat, p.poi.lon, p.poi.lat)))
                })
                .filter(|(_, d)| *d > 50.0)
                .collect();
            visible.sort_by(|a, b| snap.pois[b.0].ele.total_cmp(&snap.pois[a.0].ele));
            let info = VsInfo { lon, lat, area_km2: v.area_km2, observer: v.observer, visible, eye };
            let rect = Self::world_rect(&g);
            let _ = tx.send(JobResult::Viewshed(gen, Box::new((info, v.mask, g.w as u32, g.h as u32, rect))));
        });
    }

    fn start_live_viewshed(&mut self, lon: f64, lat: f64) {
        self.live_vs_busy = true;
        self.live_vs_at = Instant::now();
        let gen = self.gen;
        let (heights, tx, eye) = (self.heights.clone(), self.tx.clone(), self.prefs.vs_eye);
        thread::spawn(move || {
            let g = Self::grid_around(&heights, lon, lat, 18_000.0, 900.0);
            let (gx, gy) = g.to_grid(lon, lat);
            let v = analysis::viewshed(&g, gx, gy, eye, 0.0);
            let rect = Self::world_rect(&g);
            let _ = tx.send(JobResult::LiveVs(gen, Box::new((v.mask, g.w as u32, g.h as u32, rect))));
        });
    }

    fn start_rel_stats(&mut self, lon: f64, lat: f64, e: f32) {
        self.gen += 1;
        let gen = self.gen;
        let (heights, tx) = (self.heights.clone(), self.tx.clone());
        thread::spawn(move || {
            let g = Self::grid_around(&heights, lon, lat, 5_000.0, 900.0);
            let (gx, gy) = g.to_grid(lon, lat);
            let (above, below) = analysis::relative_stats(&g, gx, gy, e, 5_000.0 / g.mpp as f64);
            let _ = tx.send(JobResult::Rel(gen, RelInfo { above, below }));
        });
    }

    fn start_los(&mut self) {
        if self.measure.len() != 2 {
            self.los = None;
            return;
        }
        self.gen += 1;
        let gen = self.gen;
        let (a, b) = (self.measure[0], self.measure[1]);
        let (heights, tx) = (self.heights.clone(), self.tx.clone());
        thread::spawn(move || {
            let d = haversine(a.0, a.1, b.0, b.1);
            let pad = 500.0;
            let kx = 111_320.0 * a.1.to_radians().cos();
            let bb = BBox::new(a.0.min(b.0) - pad / kx, a.1.min(b.1) - pad / 110_574.0, a.0.max(b.0) + pad / kx, a.1.max(b.1) + pad / 110_574.0);
            let mut z = 14u8;
            while z > 9 && (d + 2.0 * pad) / crate::geo::meters_per_pixel(a.1, z) > 3000.0 {
                z -= 1;
            }
            let g = heights.grid(&bb, z);
            let r = analysis::line_of_sight(&g, g.to_grid(a.0, a.1), g.to_grid(b.0, b.1), 1.7);
            let _ = tx.send(JobResult::Los(gen, r, d));
        });
    }

    fn maybe_shadows(&mut self) {
        if !self.prefs.shadows || self.shadow_busy || self.panorama.is_some() && self.shadow_rect.is_some() {
            return;
        }
        let (az, alt) = self.sun();
        let radius = (self.cam.distance * 1.6).clamp(6_000.0, 45_000.0);
        // klucz: położenie (co ~1/4 promienia), promień (skokowo) i Słońce (co ~0,5°)
        let q = radius / 4.0;
        let (x, y) = (lon_to_x(self.cam.lon) * world_m(self.cam.lat), lat_to_y(self.cam.lat) * world_m(self.cam.lat));
        let key = ((x / q) as i64, (y / q) as i64, (radius.log2() * 2.0) as i64, ((az * 2.0) as i64) * 1000 + (alt * 2.0) as i64);
        if self.shadow_key == Some(key) {
            return;
        }
        self.shadow_key = Some(key);
        self.shadow_busy = true;
        let sunv = sun::vector(az, alt);
        let (heights, tx, lon, lat) = (self.heights.clone(), self.tx.clone(), self.cam.lon, self.cam.lat);
        let gen = self.gen;
        thread::spawn(move || {
            let g = Self::grid_around(&heights, lon, lat, radius, 1400.0);
            let m = analysis::shadows(&g, sunv);
            let rect = Self::world_rect(&g);
            let _ = tx.send(JobResult::Shadow(gen, Box::new((m, g.w as u32, g.h as u32, rect))));
        });
    }

    /// Mozaika pokrycia terenu 3×3 sektory wokół kamery (po zmianie sektora albo nowych maskach).
    fn maybe_landcover(&mut self) {
        if !self.prefs.landcover || self.land_busy || self.cam.distance > 120_000.0 {
            return;
        }
        let key = (SectorId::at(self.cam.lon, self.cam.lat), self.world.land_version());
        if self.land_key == Some(key) {
            return;
        }
        self.land_key = Some(key);
        self.land_busy = true;
        let (root, tx, heights) = (self.heights.store().root().to_path_buf(), self.tx.clone(), self.heights.clone());
        thread::spawn(move || {
            let m = crate::landcover::mosaic(&root, key.0, 1024).map(|(data, side, rect)| {
                let (lv, lw) = crate::landcover::water_levels(&data, side, rect, &heights, crate::render::NO_WATER);
                Box::new((data, side, rect, lv, lw))
            });
            let _ = tx.send(JobResult::Land(m));
        });
    }

    fn receive_jobs(&mut self) {
        while let Ok(r) = self.rx.try_recv() {
            match r {
                JobResult::Viewshed(g, b) if g == self.gen => {
                    let (info, mask, w, h, rect) = *b;
                    self.vs_busy = false;
                    if let Some(r) = &mut self.renderer {
                        r.set_viewshed(Some((&mask, w, h)));
                    }
                    self.vs_rect = Some(rect);
                    self.vs = Some(info);
                }
                JobResult::LiveVs(g, b) => {
                    self.live_vs_busy = false;
                    if g == self.gen && self.lens == Lens::Vis && self.lens_pin.is_none() {
                        let (mask, w, h, rect) = *b;
                        if let Some(r) = &mut self.renderer {
                            r.set_viewshed(Some((&mask, w, h)));
                        }
                        self.live_rect = Some(rect);
                        self.vs_rect = Some(rect);
                    }
                }
                JobResult::Shadow(_, b) => {
                    self.shadow_busy = false;
                    let (mask, w, h, rect) = *b;
                    if let Some(r) = &mut self.renderer {
                        r.set_shadow(Some((&mask, w, h)));
                    }
                    self.shadow_rect = Some(rect);
                }
                JobResult::Rel(g, info) if g == self.gen => self.rel = Some(info),
                JobResult::Los(g, r, d) if g == self.gen => self.los = Some((r, d)),
                JobResult::Land(m) => {
                    self.land_busy = false;
                    match m {
                        Some(b) => {
                            let (data, side, rect, lv, lw) = *b;
                            if let Some(r) = &mut self.renderer {
                                r.set_landcover(Some((&data, side, &lv, lw)));
                            }
                            self.land_rect = Some(rect);
                        }
                        None => self.land_rect = None,
                    }
                }
                _ => {}
            }
        }
    }

    // ------------------------------------------------------------------ soczewki

    pub fn set_lens(&mut self, l: Lens) {
        self.lens = if self.lens == l { Lens::None } else { l };
        self.unpin_lens();
    }

    pub fn pin_lens(&mut self, lon: f64, lat: f64, mode: Lens) {
        let e = self.heights.sample_cached(lon, lat).unwrap_or(0.0);
        self.lens = mode;
        self.lens_pin = Some((lon, lat, e));
        self.rel = None;
        match mode {
            Lens::Vis => self.start_viewshed(lon, lat),
            Lens::Rel => self.start_rel_stats(lon, lat, e),
            _ => {}
        }
        self.prefs.panel_open = true;
        self.tab = crate::ui::panels::Tab::Info;
    }

    pub fn unpin_lens(&mut self) {
        self.gen += 1;
        self.lens_pin = None;
        self.vs = None;
        self.vs_busy = false;
        self.rel = None;
        self.vs_rect = None;
        self.live_rect = None;
        self.live_vs = None;
        if let Some(r) = &mut self.renderer {
            r.set_viewshed(None);
        }
    }

    // ------------------------------------------------------------------ wejście

    fn ground(&self, view: &View, px: [f32; 2]) -> Option<(f64, f64, f32)> {
        let dir = view.ray(px);
        let frame = view.frame;
        let h = |x: f64, z: f64| {
            let (lon, lat) = frame.to_lonlat(x, z);
            self.heights.sample_cached(lon, lat).map(|v| v as f64)
        };
        let p = view.raycast(dir, &h)?;
        let (lon, lat) = frame.to_lonlat(p.x, p.z);
        Some((lon, lat, p.y as f32))
    }

    fn local(&self, lon: f64, lat: f64, e: f32) -> DVec3 {
        let (x, z) = self.frame.to_local(lon, lat);
        DVec3::new(x, e as f64 * self.prefs.exag, z)
    }

    fn keyboard(&mut self, ctx: &egui::Context) {
        if ctx.wants_keyboard_input() {
            return;
        }
        let (mut pan, mut rot, mut tilt, mut zoom) = (Vec2::ZERO, 0.0f64, 0.0f64, 0.0f64);
        let mut events = Vec::new();
        ctx.input(|i| {
            let dt = i.stable_dt.min(0.05) as f64;
            if i.key_down(egui::Key::ArrowLeft) {
                pan.x -= 1.0;
            }
            if i.key_down(egui::Key::ArrowRight) {
                pan.x += 1.0;
            }
            if i.key_down(egui::Key::ArrowUp) {
                pan.y -= 1.0;
            }
            if i.key_down(egui::Key::ArrowDown) {
                pan.y += 1.0;
            }
            if i.key_down(egui::Key::Q) {
                rot -= 60.0 * dt;
            }
            if i.key_down(egui::Key::E) {
                rot += 60.0 * dt;
            }
            if i.key_down(egui::Key::PageUp) {
                tilt += 40.0 * dt;
            }
            if i.key_down(egui::Key::PageDown) {
                tilt -= 40.0 * dt;
            }
            if i.key_down(egui::Key::Home) {
                zoom -= 1.5 * dt;
            }
            if i.key_down(egui::Key::End) {
                zoom += 1.5 * dt;
            }
            pan *= dt as f32;
            for e in &i.events {
                if let egui::Event::Key { key, pressed: true, modifiers, repeat: false, .. } = e {
                    events.push((*key, *modifiers));
                }
            }
        });
        if pan != Vec2::ZERO || rot != 0.0 || tilt != 0.0 || zoom != 0.0 {
            let h = self.cam.heading.to_radians();
            let k = self.cam.distance * 0.8;
            let (east, south) = (
                pan.x as f64 * h.cos() * k - pan.y as f64 * h.sin() * k,
                pan.x as f64 * h.sin() * k + pan.y as f64 * h.cos() * k,
            );
            self.move_target(east, south);
            self.cam.heading = (self.cam.heading + rot).rem_euclid(360.0);
            self.cam.pitch = (self.cam.pitch + tilt).clamp(self.min_pitch(), 89.0);
            self.cam.distance = (self.cam.distance * zoom.exp()).clamp(self.min_dist(), 900_000.0);
            self.fly = None;
            ctx.request_repaint();
        }
        let question = ctx.input(|i| i.events.iter().any(|e| matches!(e, egui::Event::Text(t) if t == "?")));
        if question {
            self.help = !self.help;
        }
        for (key, m) in events {
            if m.ctrl || m.command {
                continue;
            }
            use egui::Key as K;
            match key {
                K::Escape => {
                    if self.help {
                        self.help = false;
                    } else if self.ctx_menu.is_some() {
                        self.ctx_menu = None;
                    } else if self.panorama.is_some() {
                        self.exit_panorama();
                    } else if self.lens_pin.is_some() {
                        self.unpin_lens();
                    } else if self.lens != Lens::None {
                        self.lens = Lens::None;
                    } else if self.selection != Selection::None {
                        self.selection = Selection::None;
                    } else {
                        self.tool = Tool::Explore;
                    }
                }
                K::Num1 => self.tool = Tool::Explore,
                K::Num2 => self.tool = Tool::Measure,
                K::H | K::I => self.set_lens(Lens::Band),
                K::W => self.set_lens(Lens::Rel),
                K::O => self.set_lens(Lens::Vis),
                K::Slash => self.search = Some(crate::ui::search::Search::default()),
                K::M => {
                    // styl mapy jest w menu górnego paska; skrót przełącza na następny
                    let all = Style::ALL;
                    let i = all.iter().position(|s| *s == self.prefs.style).unwrap_or(0);
                    self.prefs.style = all[(i + 1) % all.len()];
                }
                K::P => self.prefs.panel_open = !self.prefs.panel_open,
                K::T => self.prefs.trails = !self.prefs.trails,
                K::L => self.prefs.labels = !self.prefs.labels,
                K::C => self.prefs.contours = !self.prefs.contours,
                K::N => {
                    let mut to = self.cam;
                    to.heading = 0.0;
                    self.start_fly(to, 0.7);
                }
                K::V => {
                    let mut to = self.cam;
                    to.pitch = if self.cam.pitch > 75.0 { 40.0 } else { 89.0 };
                    self.start_fly(to, 0.7);
                }
                K::R => {
                    let d = Camera::default();
                    let mut to = self.cam;
                    to.heading = d.heading;
                    to.pitch = d.pitch;
                    to.distance = d.distance.min(self.cam.distance.max(8000.0));
                    self.start_fly(to, 0.9);
                }
                K::S => self.screenshot(ctx),
                K::OpenBracket => self.shift_hour(-0.5),
                K::CloseBracket => self.shift_hour(0.5),
                K::Plus | K::Equals => self.prefs.exag = ((self.prefs.exag + 0.1) * 10.0).round() / 10.0,
                K::Minus => self.prefs.exag = (((self.prefs.exag - 0.1) * 10.0).round() / 10.0).max(1.0),
                K::Backspace | K::Delete => {
                    if self.tool == Tool::Measure && !self.measure.is_empty() {
                        self.measure.pop();
                        self.start_los();
                    }
                }
                _ => {}
            }
        }
        self.prefs.exag = self.prefs.exag.clamp(1.0, 3.0);
    }

    pub fn shift_hour(&mut self, dh: f64) {
        let (day, hour) = self.when();
        let h = (hour + dh).clamp(0.0, 23.99);
        self.prefs.custom_time = Some((day.y, day.m, day.d, h));
    }

    fn min_pitch(&self) -> f64 {
        if self.panorama.is_some() {
            -35.0
        } else {
            4.0
        }
    }

    pub fn min_dist(&self) -> f64 {
        if self.panorama.is_some() {
            1.0
        } else {
            120.0
        }
    }

    fn move_target(&mut self, east: f64, south: f64) {
        let kx = 111_320.0 * self.cam.lat.to_radians().cos();
        self.cam.lon += east / kx;
        self.cam.lat = (self.cam.lat - south / 110_574.0).clamp(-84.0, 84.0);
        self.cam.lon = (self.cam.lon + 540.0).rem_euclid(360.0) - 180.0;
    }

    /// Po obrocie przesuwa kamerę tak, żeby chwycony punkt został w tym samym miejscu ekranu.
    fn hold_orbit_point(&mut self, view: &View) {
        let Some((lon, lat, e, px)) = self.orbit else { return };
        let g = self.local(lon, lat, e);
        let next = self.cam.view(self.frame, self.prefs.exag, view.size, g.y / self.prefs.exag);
        let dir = next.ray(px);
        // punkt nad horyzontem: nie ma czego trzymać, sam obrót
        if dir.y >= -0.02 {
            return;
        }
        let hit = next.eye + dir * ((g.y - next.eye.y) / dir.y);
        let lim = self.cam.distance * 2.0;
        self.move_target((g.x - hit.x).clamp(-lim, lim), (g.z - hit.z).clamp(-lim, lim));
    }

    /// Włączone warstwy od spodu: (indeks w katalogu, krycie).
    pub fn active_layers(&self) -> Vec<(u16, f32)> {
        self.prefs
            .layers
            .iter()
            .filter(|l| l.on)
            .filter_map(|l| Some((crate::layers::index_of(&l.id)?, l.opacity)))
            .collect()
    }

    /// Czy minął czas na następną klatkę odtwarzania pogody.
    pub fn weather_tick(&mut self) -> bool {
        if self.weather_step.elapsed() < Duration::from_millis(700) {
            return false;
        }
        self.weather_step = Instant::now();
        true
    }

    /// Kamera nie wychodzi poza Polskę (z zapasem 10 km).
    fn keep_in_poland(&mut self) {
        let (lon, lat) = crate::national::clamp_to_poland(self.cam.lon, self.cam.lat);
        self.cam.lon = lon;
        self.cam.lat = lat;
    }

    fn screenshot(&mut self, ctx: &egui::Context) {
        if let Some(p) = rfd::FileDialog::new()
            .set_title("Zapisz zrzut ekranu")
            .set_file_name("psm.png")
            .add_filter("PNG", &["png"])
            .save_file()
        {
            self.screenshot = Some(p);
            ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(Default::default()));
        }
    }

    fn take_screenshot(&mut self, ctx: &egui::Context) {
        if self.screenshot.is_none() {
            return;
        }
        let shot = ctx.input(|i| {
            i.events.iter().find_map(|e| match e {
                egui::Event::Screenshot { image, .. } => Some(image.clone()),
                _ => None,
            })
        });
        if let (Some(img), Some(path)) = (shot, self.screenshot.take()) {
            let [w, h] = img.size;
            let raw: Vec<u8> = img.pixels.iter().flat_map(|c| c.to_array()).collect();
            let msg = match image::RgbaImage::from_raw(w as u32, h as u32, raw).map(|b| b.save(&path)) {
                Some(Ok(())) => format!("Zapisano zrzut: {}", path.display()),
                _ => "Nie udało się zapisać zrzutu.".to_string(),
            };
            self.notify(msg);
        }
    }

    // ------------------------------------------------------------------ klatka

    /// Główny widok 3D (w panelu centralnym).
    pub fn view_ui(&mut self, ui: &mut egui::Ui, frame: &eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.take_screenshot(&ctx);
        if self.renderer.is_none() {
            if let Some(rs) = frame.wgpu_render_state() {
                self.renderer = Some(Renderer::new(rs, self.heights.clone(), ctx.clone()));
            }
        }
        self.receive_jobs();
        self.feeds.sync(&self.prefs.feeds, &ctx);
        let snap = self.world.snapshot();
        if snap.version != self.snap.version {
            self.snap = snap;
        }
        self.keyboard(&ctx);
        self.animate(&ctx);
        self.keep_in_poland();

        // dane okolicy: sektory OSM wokół celu kamery, tylko w Polsce
        if self.last_want.elapsed() > Duration::from_millis(400) {
            self.last_want = Instant::now();
            let mut want = Vec::new();
            if self.cam.distance < 90_000.0 {
                let s = SectorId::at(self.cam.lon, self.cam.lat);
                want.push(s);
                if self.cam.distance < 60_000.0 {
                    want.extend(s.neighbours());
                }
            }
            want.retain(|s: &SectorId| crate::national::tile_in_poland(s.tile()));
            self.world.want(want);
        }

        let (rect, resp) = ui.allocate_exact_size(ui.available_size(), Sense::click_and_drag());
        let size = [rect.width().max(1.0), rect.height().max(1.0)];

        // ramka świata: przesunięcie, gdy kamera odleci daleko od początku
        let (fx, fz) = self.frame.to_local(self.cam.lon, self.cam.lat);
        if fx.hypot(fz) > 25_000.0 {
            self.frame = Frame::at(self.cam.lon, self.cam.lat);
            self.frame_version += 1;
        }
        // wysokość punktu obrotu podąża za terenem
        if self.panorama.is_none() {
            if let Some(h) = self.heights.sample_cached(self.cam.lon, self.cam.lat) {
                let k = if self.fly.is_some() { 0.08 } else { 0.25 };
                self.cam.height += (h as f64 - self.cam.height) * k;
            }
        }
        let exag = self.prefs.exag;
        let ground_eye = {
            let v = self.cam.view(self.frame, exag, size, self.cam.height);
            let (lon, lat) = self.frame.to_lonlat(v.eye.x, v.eye.z);
            self.heights.sample_cached(lon, lat).unwrap_or(0.0) as f64
        };
        // kamera nie wchodzi pod teren
        {
            let v = self.cam.view(self.frame, exag, size, ground_eye);
            let min_y = ground_eye * exag + 25.0;
            if v.eye.y < min_y && self.panorama.is_none() {
                let need = (min_y - v.target.y) / self.cam.distance.max(1.0);
                if need > 0.0 {
                    self.cam.pitch = self.cam.pitch.max(need.clamp(-1.0, 1.0).asin().to_degrees());
                }
            }
        }
        let view = self.cam.view(self.frame, exag, size, ground_eye);

        // kursor nad terenem
        let pointer = resp.hover_pos();
        let local_px = |p: Pos2| [p.x - rect.left(), p.y - rect.top()];
        self.hover = pointer.and_then(|p| self.ground(&view, local_px(p)));

        self.handle_mouse(ui, &resp, rect, &view);

        // analizy zależne od kursora i Słońca
        self.maybe_shadows();
        self.maybe_landcover();
        if self.lens == Lens::Vis && self.lens_pin.is_none() {
            if let Some((lon, lat, _)) = self.hover {
                let moved = self.live_vs.is_none_or(|(a, b)| haversine(a, b, lon, lat) > 60.0);
                if moved && !self.live_vs_busy && self.live_vs_at.elapsed() > Duration::from_millis(120) {
                    self.live_vs = Some((lon, lat));
                    self.start_live_viewshed(lon, lat);
                }
            }
        }

        let scene = self.scene(&view);
        let lines = self.dyn_lines(&view);
        let mut overlays = self.active_layers();
        if self.satellite {
            overlays.push((crate::weather::satellite_overlay(), 0.75));
        }
        if let (Some(f), Some(g)) = (self.weather_field, crate::weather::grid()) {
            let hour = self.weather_hour.unwrap_or_else(|| g.now_index());
            overlays.push((g.overlay(f, hour), 0.75));
        }
        if let Some(i) = crate::stats::overlay() {
            // kartogram pod warstwami tematycznymi
            overlays.insert(0, (i, 0.8));
        }
        let ppp = ctx.pixels_per_point();
        if let Some(r) = &mut self.renderer {
            let key = self.snap.version * 1_000_000 + self.frame_version * 2 + self.prefs.trails as u64;
            if r.static_version() != key {
                let tl = if self.prefs.trails { trail_lines(&self.snap, &self.frame, ppp) } else { Vec::new() };
                r.set_static_lines(key, &tl);
            }
            let (blit, _) = r.render(&view, &scene, &overlays, &lines, ppp);
            ui.painter().add(egui_wgpu::Callback::new_paint_callback(rect, blit));
            let [dem, sat, ortho, local] = r.tiles.loading_kinds;
            let mut parts = Vec::new();
            if dem > 0 {
                parts.push(format!("teren · {dem}"));
            }
            if sat > 0 {
                parts.push(format!("zdjęcia satelitarne · {sat}"));
            }
            if ortho > 0 {
                parts.push(format!("ortofoto · {ortho}"));
            }
            if local > 0 {
                parts.push(format!("składanie ortofoto · {local}"));
            }
            self.status_line = if parts.is_empty() { String::new() } else { format!("pobieranie: {}", parts.join(", ")) };
        }
        self.last_view = Some(view);
        self.overlays(ui, rect, &view);
        self.fps.1 += 1;
        if self.fps.0.elapsed() > Duration::from_secs(1) {
            self.fps.2 = self.fps.1 as f32 / self.fps.0.elapsed().as_secs_f32();
            self.fps = (Instant::now(), 0, self.fps.2);
        }
        if self.status_line.is_empty() && self.fly.is_none() {
            ctx.request_repaint_after(Duration::from_millis(500));
        } else {
            ctx.request_repaint();
        }
    }

    pub fn fps(&self) -> f32 {
        self.fps.2
    }

    fn handle_mouse(&mut self, ui: &mut egui::Ui, resp: &egui::Response, rect: Rect, view: &View) {
        let ctx = ui.ctx().clone();
        let to_px = |p: Pos2| [p.x - rect.left(), p.y - rect.top()];
        let pointer = resp.hover_pos().or(resp.interact_pointer_pos());
        // kółko: przybliżanie do kursora
        if let Some(p) = resp.hover_pos() {
            let scroll = ui.input(|i| i.smooth_scroll_delta.y + (i.zoom_delta() - 1.0) * 300.0) as f64;
            if scroll.abs() > 0.1 {
                self.fly = None;
                let k = (-scroll * 0.0022).exp();
                let old = self.cam.distance;
                self.cam.distance = (old * k).clamp(self.min_dist(), 900_000.0);
                if self.panorama.is_none() {
                    if let Some((lon, lat, _)) = self.ground(view, to_px(p)) {
                        let f = 1.0 - self.cam.distance / old;
                        self.cam.lon += (lon - self.cam.lon) * f;
                        self.cam.lat += (lat - self.cam.lat) * f;
                    }
                } else {
                    self.cam.fov_y = (self.cam.fov_y * k).clamp(12.0, 75.0);
                    self.cam.distance = old;
                }
            }
        }
        // przeciąganie mapy
        if resp.drag_started_by(egui::PointerButton::Primary) {
            self.drag_grab = None;
            if let Some(p) = pointer {
                    self.drag_grab = self.ground(view, to_px(p)).map(|(lon, lat, e)| self.local(lon, lat, e));
            }
        }
        if resp.dragged_by(egui::PointerButton::Primary) {
            self.fly = None;
            if self.panorama.is_some() {
                let d = resp.drag_delta();
                self.cam.heading = (self.cam.heading - d.x as f64 * 0.15).rem_euclid(360.0);
                self.cam.pitch = (self.cam.pitch - d.y as f64 * 0.12).clamp(-35.0, 60.0);
            } else if let (Some(g), Some(p)) = (self.drag_grab, pointer) {
                // chwycony punkt terenu zostaje pod kursorem
                let dir = view.ray(to_px(p));
                if dir.y < -0.02 {
                    let t = (g.y - view.eye.y) / dir.y;
                    if t > 0.0 {
                        let hit = view.eye + dir * t;
                        let (de, ds) = (g.x - hit.x, g.z - hit.z);
                        let lim = self.cam.distance * 2.0;
                        self.move_target(de.clamp(-lim, lim), ds.clamp(-lim, lim));
                    }
                } else {
                    let d = resp.drag_delta();
                    let k = self.cam.distance * 0.002;
                    let h = self.cam.heading.to_radians();
                    let (x, y) = (-d.x as f64 * k, -d.y as f64 * k);
                    self.move_target(x * h.cos() - y * h.sin(), x * h.sin() + y * h.cos());
                }
            }
        }
        if resp.drag_stopped_by(egui::PointerButton::Primary) {
            self.drag_grab = None;
        }
        // obrót i nachylenie wokół chwyconego punktu terenu (jak w Google Earth)
        if resp.drag_started_by(egui::PointerButton::Secondary) || resp.drag_started_by(egui::PointerButton::Middle) {
            self.orbit = None;
            if self.panorama.is_none() {
                if let Some(px) = ctx.input(|i| i.pointer.press_origin()).map(to_px) {
                    self.orbit = self.ground(view, px).map(|(lon, lat, e)| (lon, lat, e, px));
                }
            }
        }
        if resp.dragged_by(egui::PointerButton::Secondary) || resp.dragged_by(egui::PointerButton::Middle) {
            self.fly = None;
            let d = resp.drag_delta();
            let s = if self.panorama.is_some() { -0.15 } else { 0.3 };
            self.cam.heading = (self.cam.heading + d.x as f64 * s).rem_euclid(360.0);
            self.cam.pitch = (self.cam.pitch + d.y as f64 * 0.22).clamp(self.min_pitch(), 89.0);
            self.ctx_menu = None;
            self.hold_orbit_point(view);
        }
        if resp.drag_stopped() {
            self.orbit = None;
        }
        if resp.secondary_clicked() {
            if let (Some(p), Some((lon, lat, e))) = (pointer, self.hover) {
                self.ctx_menu = Some((p, lon, lat, e));
            }
        }
        if resp.double_clicked() {
            if let Some((lon, lat, _)) = self.hover {
                if self.panorama.is_none() {
                    self.fly_to(lon, lat, Some((self.cam.distance / 2.5).max(600.0)));
                }
            }
            return;
        }
        if resp.clicked() {
            self.ctx_menu = None;
            let Some((lon, lat, _)) = self.hover else { return };
            // punkt danych na żywo pod kursorem
            if let (Some(p), Tool::Explore, Lens::None) = (pointer, self.tool, self.lens) {
                let hit = self.feed_hits.iter().find(|h| (h.0 - p).length() <= h.1 + 4.0).map(|h| (h.2, h.3.clone()));
                if let Some((id, key)) = hit {
                    self.selection = Selection::Feed(id.to_string(), key);
                    self.prefs.panel_open = true;
                    self.tab = crate::ui::panels::Tab::Info;
                    return;
                }
            }
            // etykieta pod kursorem
            if let Some(p) = pointer {
                if let Some(i) = self.label_hit(p, rect, view) {
                    if self.tool == Tool::Explore && self.lens == Lens::None {
                        self.selection = Selection::Poi(i);
                        self.prefs.panel_open = true;
                        self.tab = crate::ui::panels::Tab::Info;
                        return;
                    }
                }
            }
            if self.lens != Lens::None && self.lens_pin.is_none() {
                self.pin_lens(lon, lat, self.lens);
                return;
            }
            match self.tool {
                Tool::Explore => {
                    // szlak w pobliżu kliknięcia (promień zależny od odległości)
                    let r = (self.cam.distance * 0.006).clamp(15.0, 400.0);
                    // przy włączonych warstwach klik pyta o miejsce, a nie wybiera szlaku
                    if self.prefs.trails && self.active_layers().is_empty() {
                        if let Some(s) = self.snap.graph.snap(lon, lat, r) {
                            self.selection = Selection::Trail(s.edge);
                            self.prefs.panel_open = true;
                            self.tab = crate::ui::panels::Tab::Info;
                            return;
                        }
                    }
                    self.selection = Selection::Point { lon, lat };
                    self.prefs.panel_open = true;
                    self.tab = crate::ui::panels::Tab::Info;
                }
                Tool::Measure => {
                    let e = self.heights.sample_cached(lon, lat).unwrap_or(0.0);
                    self.measure.push((lon, lat, e));
                    self.start_los();
                }
            }
        }
        let _ = ctx;
    }

    fn label_hit(&self, p: Pos2, rect: Rect, view: &View) -> Option<usize> {
        if !self.prefs.labels {
            return None;
        }
        for (i, poi) in self.snap.pois.iter().enumerate().take(600) {
            if let Some(s) = view.project(self.local(poi.poi.lon, poi.poi.lat, poi.dem)) {
                let sp = Pos2::new(s[0] + rect.left(), s[1] + rect.top());
                if (sp - p).length() < 9.0 {
                    return Some(i);
                }
            }
        }
        None
    }

    fn scene(&self, view: &View) -> Scene {
        let (az, alt) = self.sun();
        let sv = sun::vector(az, alt);
        let (day, _) = self.when();
        // barwy: ciepłe nisko nad horyzontem, nocą chłodne i ciemne
        let a = alt as f32;
        let day_k = ((a + 4.0) / 14.0).clamp(0.0, 1.0);
        let warm = 1.0 - ((a - 2.0) / 25.0).clamp(0.0, 1.0);
        let sun_color = [
            (1.0 + 0.25 * warm) * 2.2 * day_k,
            (0.96 - 0.25 * warm) * 2.1 * day_k,
            (0.88 - 0.5 * warm) * 1.95 * day_k,
        ];
        let night = [0.05, 0.07, 0.12];
        let mixc = |d: [f32; 3], n: [f32; 3]| [n[0] + (d[0] - n[0]) * day_k, n[1] + (d[1] - n[1]) * day_k, n[2] + (d[2] - n[2]) * day_k];
        let sky = mixc([0.32, 0.42, 0.58], night);
        let zenith = mixc([0.16, 0.32, 0.62], [0.01, 0.015, 0.035]);
        let horizon_day = [0.62 + 0.25 * warm, 0.70 + 0.05 * warm, 0.80 - 0.25 * warm];
        let horizon = mixc(horizon_day, [0.03, 0.04, 0.07]);
        let lens_mode = match self.lens {
            Lens::None => 0.0,
            Lens::Band => if self.prefs.band_tol > 0.0 { 2.0 } else { 1.0 },
            Lens::Rel => 3.0,
            Lens::Vis => 0.0,
        };
        let lens_src = self.lens_pin.or(self.hover);
        let lens = match (lens_src, lens_mode > 0.0) {
            (Some((lon, lat, e)), true) => {
                let (x, z) = self.frame.to_local(lon, lat);
                [x as f32, z as f32, e, lens_mode]
            }
            _ => [0.0, 0.0, 0.0, 0.0],
        };
        let cursor = if self.lens == Lens::None {
            self.hover.map(|(lon, lat, e)| {
                let (x, z) = self.frame.to_local(lon, lat);
                [x as f32, z as f32, e]
            })
        } else {
            None
        };
        let to_rect = |r: Option<[f64; 4]>| -> [f32; 4] {
            match r {
                Some([x0, y0, x1, y1]) => {
                    let (a, b) = self.frame.world_to_local(x0, y0);
                    let (c, d) = self.frame.world_to_local(x1, y1);
                    [a as f32, b as f32, (1.0 / (c - a)) as f32, (1.0 / (d - b)) as f32]
                }
                None => [0.0; 4],
            }
        };
        // piętra roślinności i śniegu: wyżej bliżej równika (Alpy ~1800 m, Himalaje ~4000 m), niżej na północy
        let lift = (49.5 - self.cam.lat.abs() as f32).clamp(-20.0, 25.0);
        let up = if lift > 0.0 { lift * 120.0 } else { lift * 60.0 };
        let forest = 1300.0 + up;
        let biome = [forest, forest + 180.0 + lift.max(0.0) * 15.0, forest + 350.0 + lift.max(0.0) * 30.0];
        let snow_lift = if lift > 0.0 { lift * 150.0 } else { lift * 60.0 };
        // cięcie poziomic zależne od odległości
        let d = self.cam.distance;
        let contour = if d < 2500.0 { 10.0 } else if d < 9000.0 { 20.0 } else if d < 30_000.0 { 50.0 } else { 100.0 };
        let _ = view;
        Scene {
            style: self.prefs.style.code(),
            contours: self.prefs.contours,
            contour_interval: contour,
            km_grid: self.prefs.km_grid,
            sun_dir: sv,
            sun_power: if alt > -2.0 { 1.0 } else { 0.0 },
            sun_color,
            sky_color: sky,
            zenith,
            horizon,
            // z wysoka mgła słabnie (inaczej przy dużym oddaleniu cały widok jest mleczny)
            fog_density: 1.0 / (95_000.0 * (self.cam.distance / 30_000.0).max(1.0) as f32),
            snowline: sun::snowline(day) as f32 + snow_lift,
            snow: self.prefs.snow,
            autumn: sun::autumn(day),
            lens,
            band_tol: self.prefs.band_tol,
            rel_range: self.prefs.rel_range,
            cursor,
            shadows: self.prefs.shadows && self.shadow_rect.is_some(),
            viewshed: self.lens == Lens::Vis && self.vs_rect.is_some(),
            biome,
            render_scale: self.prefs.render_scale,
            detail_px: self.prefs.detail_px,
            imagery: self.prefs.style == Style::Satellite,
            ortho: self.prefs.ortho,
            detail: self.prefs.detail,
            max_dem: self.prefs.max_dem,
            max_img: self.prefs.max_img,
            shadow_rect: to_rect(self.shadow_rect),
            vs_rect: to_rect(self.vs_rect),
            line_fade: (self.cam.distance as f32 * 3.0).clamp(15_000.0, 90_000.0),
            landcover: self.prefs.landcover && self.land_rect.is_some(),
            land_rect: to_rect(self.land_rect),
        }
    }

    /// Linie zmienne: pomiar i linia wzroku.
    fn dyn_lines(&self, view: &View) -> Vec<LineInst> {
        let mut out = Vec::new();
        let _ = view;
        // pomiar: linia po terenie
        if self.measure.len() >= 2 {
            for w in self.measure.windows(2) {
                let n = (haversine(w[0].0, w[0].1, w[1].0, w[1].1) / 30.0).ceil().clamp(2.0, 400.0) as usize;
                let mut prev: Option<[f32; 3]> = None;
                for k in 0..=n {
                    let f = k as f64 / n as f64;
                    let lon = w[0].0 + (w[1].0 - w[0].0) * f;
                    let lat = w[0].1 + (w[1].1 - w[0].1) * f;
                    let e = self.heights.sample_cached(lon, lat).unwrap_or(0.0);
                    let (x, z) = self.frame.to_local(lon, lat);
                    let p = [x as f32, e + 2.0, z as f32];
                    if let Some(q) = prev {
                        out.push(LineInst { p0: q, p1: p, color: [1.0, 1.0, 1.0, 1.0], wo: [3.0, 0.0] });
                    }
                    prev = Some(p);
                }
            }
            if self.measure.len() == 2 {
                if let Some((vis, _)) = self.los {
                    let (a, b) = (self.measure[0], self.measure[1]);
                    let pa = self.local(a.0, a.1, a.2 + 1.7);
                    let pb = self.local(b.0, b.1, b.2 + 1.7);
                    let c = if vis.is_none() { lin([110, 230, 120], 1.0) } else { lin([255, 90, 54], 1.0) };
                    let ex = self.prefs.exag;
                    out.push(LineInst {
                        p0: [pa.x as f32, (pa.y / ex) as f32, pa.z as f32],
                        p1: [pb.x as f32, (pb.y / ex) as f32, pb.z as f32],
                        color: c,
                        wo: [2.0, 0.0],
                    });
                }
            }
        }
        out
    }

    /// Etykiety, znaczniki, kompas, podziałka, odczyt kursora i menu kontekstowe.
    fn overlays(&mut self, ui: &mut egui::Ui, rect: Rect, view: &View) {
        let p = ui.painter_at(rect);
        let at = |s: [f32; 2]| Pos2::new(s[0] + rect.left(), s[1] + rect.top());
        // etykiety punktów z rozrzedzaniem i zasłanianiem przez teren
        if self.prefs.labels {
            let mut taken: Vec<Rect> = Vec::new();
            let max = (70.0 * self.prefs.label_density) as usize;
            let mut shown = 0;
            for poi in self.snap.pois.iter().take(2500) {
                if shown >= max {
                    break;
                }
                let pos = self.local(poi.poi.lon, poi.poi.lat, poi.dem);
                let Some(s) = view.project(pos) else { continue };
                let sp = at(s);
                if !rect.shrink(4.0).contains(sp) {
                    continue;
                }
                let dist = pos.distance(view.eye);
                // drobne punkty tylko z bliska
                let min_prom = (dist / 9000.0) as f32;
                if poi.prom < min_prom.min(30.0) * 0.6 {
                    continue;
                }
                if self.occluded(view, pos) {
                    continue;
                }
                let (label, color, size) = match poi.poi.kind {
                    PoiKind::Peak => (format!("{}  {}", poi.poi.name, fmt::int(poi.ele as f64)), theme::PAPER, 12.5),
                    PoiKind::Place => (poi.poi.name.clone(), Color32::from_rgb(230, 222, 200), if poi.poi.place == "city" || poi.poi.place == "town" { 13.5 } else { 11.5 }),
                    PoiKind::Hut | PoiKind::Shelter => (poi.poi.name.clone(), Color32::from_rgb(255, 214, 150), 11.5),
                    PoiKind::Lake => (poi.poi.name.clone(), Color32::from_rgb(150, 205, 240), 11.0),
                    _ => (poi.poi.name.clone(), theme::PAPER_2, 11.0),
                };
                let font = FontId::proportional(size);
                let galley = p.layout_no_wrap(label, font, color);
                let lr = Rect::from_min_size(sp + Vec2::new(7.0, -galley.size().y - 3.0), galley.size()).expand(2.0);
                if taken.iter().any(|t| t.intersects(lr)) {
                    continue;
                }
                taken.push(lr);
                shown += 1;
                let dot = match poi.poi.kind {
                    PoiKind::Peak => theme::PAPER,
                    PoiKind::Hut | PoiKind::Shelter => Color32::from_rgb(255, 170, 70),
                    PoiKind::Lake => Color32::from_rgb(110, 180, 230),
                    PoiKind::Place => Color32::from_rgb(200, 190, 170),
                    _ => theme::PAPER_2,
                };
                p.line_segment([sp, sp + Vec2::new(5.0, -5.0)], Stroke::new(1.0, Color32::from_black_alpha(160)));
                if poi.poi.kind == PoiKind::Peak {
                    let tri = vec![sp + Vec2::new(0.0, -5.0), sp + Vec2::new(4.5, 3.0), sp + Vec2::new(-4.5, 3.0)];
                    p.add(egui::Shape::convex_polygon(tri, dot, Stroke::new(1.0, Color32::from_black_alpha(200))));
                } else {
                    p.circle(sp, 3.5, dot, Stroke::new(1.0, Color32::from_black_alpha(200)));
                }
                let tp = lr.min + Vec2::splat(2.0);
                // obwódka dla czytelności na jasnym terenie
                for o in [Vec2::new(1.0, 1.0), Vec2::new(-1.0, 1.0), Vec2::new(1.0, -1.0), Vec2::new(-1.0, -1.0)] {
                    p.galley_with_override_text_color(tp + o, galley.clone(), Color32::from_black_alpha(170));
                }
                p.galley(tp, galley, color);
                if self.panorama.is_some() {
                    let d = haversine(self.cam.lon, self.cam.lat, poi.poi.lon, poi.poi.lat);
                    p.text(lr.left_bottom() + Vec2::new(2.0, 1.0), Align2::LEFT_TOP, fmt::dist(d), FontId::monospace(10.0), theme::PAPER_3);
                }
            }
        }
        // słupki 3D kartogramu: wysokość z wartości, dalsze rysowane pierwsze
        if self.stats_bars {
            if let Some(m) = crate::stats::map() {
                let max = m.values.values().fold(f64::MIN, |a, b| a.max(*b)).max(1e-9);
                let top_m = match m.level {
                    crate::stats::Level::Voivodeship => 90_000.0,
                    crate::stats::Level::County => 50_000.0,
                    crate::stats::Level::Commune => 25_000.0,
                };
                let mut bars: Vec<(f64, Pos2, Pos2, Color32)> = Vec::new();
                for u in crate::stats::units(m.level) {
                    let Some(v) = m.values.get(&u.teryt) else { continue };
                    let e = self.heights.sample_cached(u.center[0], u.center[1]).unwrap_or(0.0);
                    let base = self.local(u.center[0], u.center[1], e);
                    let top = base + DVec3::Y * (v / max).max(0.0) * top_m;
                    let (Some(a), Some(b)) = (view.project(base), view.project(top)) else { continue };
                    let c = crate::stats::PALETTE[crate::stats::class_of(*v, &m.breaks)];
                    bars.push((base.distance(view.eye), at(a), at(b), Color32::from_rgb(c[0], c[1], c[2])));
                }
                bars.sort_by(|x, y| y.0.total_cmp(&x.0));
                let w = (2_400_000.0 / self.cam.distance.max(1.0)).clamp(3.0, 14.0) as f32;
                for (_, a, b, c) in bars {
                    if rect.contains(a) {
                        p.line_segment([a, b], Stroke::new(w + 2.0, Color32::from_black_alpha(170)));
                        p.line_segment([a, b], Stroke::new(w, c));
                    }
                }
            }
        }
        // cząstki wiatru: krótkie kreski płynące z polem wiatru wybranej godziny
        if self.wind_particles {
            if let Some(g) = crate::weather::grid() {
                let hour = self.weather_hour.unwrap_or_else(|| g.now_index());
                let dt = ui.input(|i| i.stable_dt).min(0.05) as f64;
                if self.particles.is_empty() {
                    // rozkład pseudolosowy o stałym wzorze (złoty podział)
                    self.particles = (0..900)
                        .map(|i| {
                            let (a, b) = ((i as f64 * 0.618_034).fract(), (i as f64 * 0.754_878).fract());
                            (14.0 + a * 10.2, 49.0 + b * 5.9, (i % 90) as f32)
                        })
                        .collect();
                }
                let mut lines = Vec::new();
                for (k, q) in self.particles.iter_mut().enumerate() {
                    q.2 += 1.0;
                    let (fa, fb) = ((k as f64 * 0.618_034 + q.2 as f64 * 0.013).fract(), (k as f64 * 0.754_878 + q.2 as f64 * 0.017).fract());
                    if q.2 > 110.0 || !crate::national::in_poland(q.0, q.1) {
                        *q = (14.0 + fa * 10.2, 49.0 + fb * 5.9, 0.0);
                        continue;
                    }
                    let Some((u, v)) = g.wind(hour, q.0, q.1) else { continue };
                    // przyspieszenie, żeby ruch był widoczny w skali kraju
                    q.0 += u as f64 * dt * 0.012 / q.1.to_radians().cos();
                    q.1 += v as f64 * dt * 0.012;
                    let tail = (q.0 - u as f64 * 0.006 / q.1.to_radians().cos(), q.1 - v as f64 * 0.006);
                    lines.push((*q, tail));
                }
                for (a, b) in lines {
                    let e = self.heights.sample_cached(a.0, a.1).unwrap_or(0.0);
                    if let (Some(s0), Some(s1)) = (view.project(self.local(a.0, a.1, e)), view.project(self.local(b.0, b.1, e))) {
                        p.line_segment([at(s0), at(s1)], Stroke::new(1.3, Color32::from_white_alpha(170)));
                    }
                }
                ui.ctx().request_repaint();
            }
        }
        // dane na żywo: rozmiar i barwa z wartości, przy zagęszczeniu zostają najważniejsze
        let mut hits = Vec::new();
        for def in &crate::feeds::FEEDS {
            if !self.prefs.feeds.iter().any(|f| f == def.id) {
                continue;
            }
            let mut cells = std::collections::HashSet::new();
            let close = self.cam.distance < 80_000.0;
            for pt in self.feeds.points(def.id).iter() {
                let e = self.heights.sample_cached(pt.lon, pt.lat).unwrap_or(0.0);
                let Some(s) = view.project(self.local(pt.lon, pt.lat, e)) else { continue };
                let sp = at(s);
                if !rect.contains(sp) || !cells.insert(((sp.x / 18.0) as i32, (sp.y / 18.0) as i32)) {
                    continue;
                }
                let c = Color32::from_rgb(pt.color[0], pt.color[1], pt.color[2]);
                p.circle(sp, pt.radius, c, Stroke::new(1.0, Color32::from_black_alpha(200)));
                if close && !pt.label.is_empty() {
                    p.text(sp + Vec2::new(pt.radius + 3.0, 0.0), Align2::LEFT_CENTER, &pt.label, FontId::proportional(10.5), theme::PAPER);
                }
                hits.push((sp, pt.radius, def.id, pt.key.clone()));
            }
        }
        self.feed_hits = hits;
        for (i, m) in self.measure.iter().enumerate() {
            if let Some(s) = view.project(self.local(m.0, m.1, m.2)) {
                let sp = at(s);
                p.circle(sp, 6.0, Color32::WHITE, Stroke::new(2.0, Color32::BLACK));
                p.text(sp + Vec2::new(9.0, -9.0), Align2::LEFT_BOTTOM, (i + 1).to_string(), FontId::proportional(11.0), Color32::WHITE);
            }
        }
        if let Some((lon, lat, e, _)) = self.orbit {
            if let Some(s) = view.project(self.local(lon, lat, e)) {
                p.circle_stroke(at(s), 6.0, Stroke::new(2.0, theme::PAPER));
            }
        }
        if let Some((lon, lat, e)) = self.lens_pin {
            if let Some(s) = view.project(self.local(lon, lat, e)) {
                p.circle_stroke(at(s), 8.0, Stroke::new(2.5, theme::BRASS));
                p.circle_filled(at(s), 3.0, theme::BRASS);
            }
        }
        match &self.selection {
            Selection::Point { lon, lat } => {
                let e = self.heights.sample_cached(*lon, *lat).unwrap_or(0.0);
                if let Some(s) = view.project(self.local(*lon, *lat, e)) {
                    p.circle_stroke(at(s), 7.0, Stroke::new(2.0, theme::TEAL));
                }
            }
            Selection::Poi(i) => {
                if let Some(poi) = self.snap.pois.get(*i) {
                    if let Some(s) = view.project(self.local(poi.poi.lon, poi.poi.lat, poi.dem)) {
                        p.circle_stroke(at(s), 9.0, Stroke::new(2.0, theme::TEAL));
                    }
                }
            }
            _ => {}
        }

        // kompas
        let r = 30.0;
        let c = rect.right_bottom() + Vec2::new(-51.0, -322.0);
        let compass = Rect::from_center_size(c, Vec2::splat(r * 2.0 + 4.0));
        let resp = ui.interact(compass, ui.id().with("kompas"), Sense::click_and_drag());
        let hot = resp.hovered() || resp.dragged();
        p.circle_filled(c + Vec2::new(0.0, 2.0), r + 1.0, Color32::from_black_alpha(90));
        p.circle(
            c,
            r,
            Color32::from_rgba_unmultiplied(26, 29, 35, 242),
            Stroke::new(1.0, if hot { theme::PAPER_3 } else { theme::INK_4 }),
        );
        // kierunek patrzenia: stały znacznik u góry tarczy
        let top = c - Vec2::new(0.0, r);
        p.add(egui::Shape::convex_polygon(
            vec![top + Vec2::new(0.0, 6.0), top + Vec2::new(-4.0, -1.0), top + Vec2::new(4.0, -1.0)],
            theme::BRASS,
            Stroke::NONE,
        ));
        // tarcza obraca się z kamerą: podziałka co 15°, główne kierunki co 90°
        let rot = |deg: f32| {
            let a = (deg - self.cam.heading as f32).to_radians();
            Vec2::new(a.sin(), -a.cos())
        };
        for i in 0..24 {
            let deg = i as f32 * 15.0;
            let d = rot(deg);
            let (len, w, col) = match i % 6 {
                0 => (0.0, 0.0, theme::PAPER),
                _ if i % 2 == 0 => (5.0, 1.2, theme::PAPER_3),
                _ => (3.0, 1.0, theme::INK_4),
            };
            if len > 0.0 {
                p.line_segment([c + d * (r - 2.0), c + d * (r - 2.0 - len)], Stroke::new(w, col));
            }
        }
        for (deg, s) in [(90.0, "E"), (180.0, "S"), (270.0, "W")] {
            p.text(c + rot(deg) * (r - 9.0), Align2::CENTER_CENTER, s, FontId::proportional(9.5), theme::PAPER_3);
        }
        p.text(c + rot(0.0) * (r - 9.0), Align2::CENTER_CENTER, "N", FontId::proportional(11.0), theme::SIGNAL);
        // igła: czerwona połowa na północ, jasna na południe, pochylona razem z kamerą
        let n = rot(0.0);
        let side = Vec2::new(-n.y, n.x) * 3.5;
        let tip = 15.0 * (0.45 + 0.55 * (self.cam.pitch as f32).to_radians().sin().abs()).min(1.0);
        p.add(egui::Shape::convex_polygon(vec![c + n * tip, c + side, c - side], theme::SIGNAL, Stroke::NONE));
        p.add(egui::Shape::convex_polygon(vec![c - n * tip, c - side, c + side], theme::PAPER_3, Stroke::NONE));
        p.circle(c, 2.5, Color32::from_rgb(26, 29, 35), Stroke::new(1.0, theme::PAPER_3));
        // odczyt kursu pod tarczą
        let hdg = (self.cam.heading.rem_euclid(360.0).round() as i32) % 360;
        const DIRS: [&str; 8] = ["N", "NE", "E", "SE", "S", "SW", "W", "NW"];
        let label = format!("{hdg:03}° {}", DIRS[((hdg as f32 + 22.5) / 45.0) as usize % 8]);
        let lp = c + Vec2::new(0.0, r + 11.0);
        let galley = p.layout_no_wrap(label, FontId::monospace(10.5), theme::PAPER);
        let bg = Rect::from_center_size(lp, galley.size() + Vec2::new(10.0, 4.0));
        p.rect_filled(bg, 4.0, Color32::from_rgba_unmultiplied(26, 29, 35, 230));
        p.galley(bg.center() - galley.size() / 2.0, galley, theme::PAPER);
        // klik: północ; dwuklik: północ i widok z góry; przeciągnięcie w poziomie: obrót
        if resp.dragged() {
            self.cam.heading = (self.cam.heading - resp.drag_delta().x as f64 * 1.2).rem_euclid(360.0);
        }
        if resp.double_clicked() {
            let mut to = self.cam;
            to.heading = 0.0;
            to.pitch = 89.0;
            self.start_fly(to, 0.7);
        } else if resp.clicked() {
            let mut to = self.cam;
            to.heading = 0.0;
            self.start_fly(to, 0.7);
        }
        resp.on_hover_text("Klik: północ u góry (N)\nDwuklik: północ i widok z góry\nPrzeciągnij w bok: obrót");

        // podziałka
        let mpp = view.eye.distance(view.target) * (self.cam.fov_y.to_radians() / 2.0).tan() * 2.0 / rect.height() as f64;
        let target = mpp * 120.0;
        let nice = [1.0, 2.0, 5.0]
            .iter()
            .flat_map(|m| (0..7).map(move |e| m * 10f64.powi(e)))
            .filter(|v| *v <= target)
            .fold(1.0, f64::max);
        let px = (nice / mpp) as f32;
        let base = rect.left_bottom() + Vec2::new(16.0, -40.0);
        p.line_segment([base, base + Vec2::new(px, 0.0)], Stroke::new(2.0, theme::PAPER));
        p.line_segment([base, base - Vec2::new(0.0, 5.0)], Stroke::new(2.0, theme::PAPER));
        p.line_segment([base + Vec2::new(px, 0.0), base + Vec2::new(px, -5.0)], Stroke::new(2.0, theme::PAPER));
        p.text(base + Vec2::new(px + 6.0, -2.0), Align2::LEFT_CENTER, fmt::dist(nice), FontId::monospace(11.0), theme::PAPER);

        // odczyt kursora
        let mut info = String::new();
        if let Some((lon, lat, e)) = self.hover {
            let g = |dx: f64, dy: f64| self.heights.sample_cached(lon + dx, lat + dy).unwrap_or(e) as f64;
            let kx = 111_320.0 * lat.to_radians().cos();
            let step = 15.0;
            let gx = (g(step / kx, 0.0) - g(-step / kx, 0.0)) / (2.0 * step);
            let gy = (g(0.0, step / 110_574.0) - g(0.0, -step / 110_574.0)) / (2.0 * step);
            let slope = gx.hypot(gy).atan().to_degrees();
            let aspect = (-gx).atan2(-gy).to_degrees().rem_euclid(360.0);
            info = format!(
                "{}   {}   nachylenie {}°   ekspozycja {}",
                fmt::coords(lon, lat),
                fmt::ele(e as f64),
                slope.round(),
                fmt::compass(aspect)
            );
            if let Some((rl, rt, re)) = self.lens_pin.filter(|_| self.lens == Lens::Rel) {
                let d = haversine(rl, rt, lon, lat);
                let dh = e - re;
                info.push_str(&format!(
                    "   Δh {}   {}   kąt {}°   azymut {}°",
                    fmt::signed(dh as f64, "m"),
                    fmt::dist(d),
                    fmt::dec((dh as f64 / d.max(1.0)).atan().to_degrees(), 1),
                    bearing(rl, rt, lon, lat).round()
                ));
            }
        }
        let bar = Rect::from_min_max(rect.left_bottom() - Vec2::new(0.0, 24.0), rect.right_bottom());
        p.rect_filled(bar, 0.0, Color32::from_black_alpha(130));
        p.text(bar.left_center() + Vec2::new(12.0, 0.0), Align2::LEFT_CENTER, info, FontId::monospace(11.5), theme::PAPER_2);
        let attr = match self.prefs.style {
            Style::Satellite => "Sentinel-2 cloudless 2020 © EOX · ortofoto i NMT © GUGiK · teren © Mapterhorn · © OpenStreetMap",
            _ => "Teren © GUGiK, Mapterhorn, AWS Open Data · © OpenStreetMap",
        };
        // atrybucja włączonych warstw (wymagana licencjami)
        let mut sources: Vec<&str> = Vec::new();
        if self.weather_field.is_some() || self.wind_particles {
            sources.push("Open-Meteo (CC BY 4.0), użytek niekomercyjny");
        }
        if self.satellite {
            sources.push("© EUMETSAT");
        }
        if crate::stats::map().is_some() {
            sources.push("GUS BDL · PRG © GUGiK");
        }
        for def in &crate::feeds::FEEDS {
            if self.prefs.feeds.iter().any(|f| f == def.id) && !sources.contains(&def.attribution) {
                sources.push(def.attribution);
            }
        }
        for (i, _) in self.active_layers() {
            let a = crate::layers::catalog().layers[i as usize].attribution.as_str();
            if !sources.contains(&a) {
                sources.push(a);
            }
        }
        let attr = if sources.is_empty() { attr.to_string() } else { format!("{} · {attr}", sources.join(" · ")) };
        p.text(bar.right_center() - Vec2::new(12.0, 0.0), Align2::RIGHT_CENTER, attr, FontId::proportional(10.5), theme::PAPER_3);

        // stan wczytywania i tryby
        let mut chips: Vec<String> = Vec::new();
        match self.world.status() {
            (Some(_), _) => chips.push("pobieranie nazw i szlaków…".into()),
            (_, Some(e)) => chips.push(format!("nazwy i szlaki: {}", short(&e))),
            _ => {}
        }
        if !self.status_line.is_empty() {
            chips.push(self.status_line.clone());
        }
        if self.world.land_busy() {
            chips.push("pobieranie pokrycia terenu (OSM)…".into());
        }
        if self.land_busy {
            chips.push("wypiekanie pokrycia terenu i poziomów wody…".into());
        }
        if self.shadow_busy {
            chips.push("liczenie cieni…".into());
        }
        if self.vs_busy {
            chips.push("liczenie widoczności…".into());
        }
        if self.panorama.is_some() {
            chips.push("panorama · Esc – powrót".into());
        }
        let lens_txt = match self.lens {
            Lens::Band => Some("soczewka: ta sama wysokość"),
            Lens::Rel => Some("soczewka: wysokość względna"),
            Lens::Vis => Some("soczewka: widoczność"),
            Lens::None => None,
        };
        if let Some(t) = lens_txt {
            chips.push(format!("{t}{}", if self.lens_pin.is_some() { " (przypięta · Esc)" } else { " · klik przypina" }));
        }
        // statusy na środku dolnego paska (nad odczytem kursora i atrybucją)
        if !chips.is_empty() {
            let g = p.layout_no_wrap(chips.join("  ·  "), FontId::proportional(11.5), theme::PAPER);
            let r = Rect::from_center_size(bar.center(), Vec2::new(g.size().x + 20.0, bar.height() - 4.0));
            p.rect_filled(r, 9.0, Color32::from_rgba_unmultiplied(26, 29, 35, 245));
            p.galley(r.center() - g.size() / 2.0, g, theme::PAPER);
        }

        // menu kontekstowe
        if let Some((pos, lon, lat, e)) = self.ctx_menu {
            let mut close = false;
            egui::Area::new(egui::Id::new("menu-3d")).fixed_pos(pos).order(egui::Order::Foreground).show(ui.ctx(), |ui| {
                theme::card(false).show(ui, |ui| {
                    ui.set_width(230.0);
                    ui.label(egui::RichText::new(format!("{}  ·  {}", fmt::decimal(lon, lat), fmt::ele(e as f64))).small().monospace().color(theme::PAPER_3));
                    let mut item = |ui: &mut egui::Ui, t: &str| ui.add(egui::Button::new(t).frame(false)).clicked();
                    if item(ui, "Ta sama wysokość") {
                        self.pin_lens(lon, lat, Lens::Band);
                        close = true;
                    }
                    if item(ui, "Wysokość względna") {
                        self.pin_lens(lon, lat, Lens::Rel);
                        close = true;
                    }
                    if item(ui, "Co widać stąd?") {
                        self.pin_lens(lon, lat, Lens::Vis);
                        close = true;
                    }
                    if item(ui, "Panorama stąd") {
                        self.enter_panorama(lon, lat);
                        close = true;
                    }
                    if item(ui, "Pomiar stąd") {
                        self.tool = Tool::Measure;
                        self.measure = vec![(lon, lat, e)];
                        self.los = None;
                        close = true;
                    }
                    ui.separator();
                    if item(ui, "Informacje o punkcie") {
                        self.tool = Tool::Explore;
                        self.selection = Selection::Point { lon, lat };
                        self.prefs.panel_open = true;
                        close = true;
                    }
                    if item(ui, "Kopiuj współrzędne") {
                        ui.ctx().copy_text(fmt::decimal(lon, lat));
                        self.notify("Skopiowano współrzędne.");
                        close = true;
                    }
                });
            });
            if close {
                self.ctx_menu = None;
            }
        }

        // komunikat
        let expired = self.toast.as_ref().is_some_and(|(_, t)| t.elapsed() >= Duration::from_secs(5));
        if expired {
            self.toast = None;
        }
        if let Some((msg, _)) = &self.toast {
            {
                let g = p.layout(msg.clone(), FontId::proportional(13.0), theme::PAPER, 460.0);
                let r = Rect::from_center_size(rect.center_bottom() - Vec2::new(0.0, 70.0), g.size() + Vec2::new(28.0, 18.0));
                p.rect(r, 10.0, theme::INK_2, Stroke::new(1.0, theme::BRASS_DIM), egui::StrokeKind::Inside);
                p.galley(r.min + Vec2::new(14.0, 9.0), g, theme::PAPER);
                ui.ctx().request_repaint_after(Duration::from_millis(300));
            }
        }
    }

    /// Czy teren zasłania punkt (próbkowanie wzdłuż linii z oka, z poziomu ~38 m).
    fn occluded(&self, view: &View, p: DVec3) -> bool {
        let e = view.eye;
        let n = 14;
        for k in 1..n {
            let f = k as f64 / n as f64;
            let q = e + (p - e) * f;
            let (lon, lat) = self.frame.to_lonlat(q.x, q.z);
            if let Some(h) = self.heights.sample_at(lon, lat, 12, false) {
                let hd = (q - e).truncate().length();
                let drop = hd * hd / (2.0 * crate::geo::EARTH_R) * 0.87;
                if h as f64 * self.prefs.exag - drop > q.y + 15.0 {
                    return true;
                }
            }
        }
        false
    }
}

fn short(s: &str) -> String {
    let mut out: String = s.chars().take(60).collect();
    if s.chars().count() > 60 {
        out.push('…');
    }
    out
}

/// Linie szlaków z grafu: kolory relacji (kilka kolorów – równoległe pasy).
fn trail_lines(snap: &Snapshot, frame: &Frame, ppp: f32) -> Vec<LineInst> {
    let mut out = Vec::new();
    let w = 2.6 * ppp.max(1.0);
    for ed in &snap.graph.edges {
        let n = ed.n();
        // co ~25 m wystarczy do rysowania
        let step = 2usize;
        let idx: Vec<usize> = (0..n).step_by(step).chain(std::iter::once(n - 1)).collect();
        let pts: Vec<[f32; 3]> = idx
            .iter()
            .map(|&i| {
                let (x, z) = frame.to_local(ed.lon[i], ed.lat[i]);
                [x as f32, ed.e[i] + 1.0, z as f32]
            })
            .collect();
        let k = ed.colors.len() as f32;
        for (ci, c) in ed.colors.iter().enumerate() {
            let off = (ci as f32 - (k - 1.0) / 2.0) * w;
            let mut col = lin(c.rgb(), 0.98);
            if *c == crate::metrics::TrailColor::Other {
                col = lin([214, 196, 160], 0.98);
            }
            for p in pts.windows(2) {
                if p[0] == p[1] {
                    continue;
                }
                out.push(LineInst { p0: p[0], p1: p[1], color: col, wo: [w, off] });
            }
        }
    }
    out
}
