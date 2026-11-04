//! Pogoda: siatka Open-Meteo nad Polską z osią czasu, pola barwne rysowane jak warstwy
//! (kafle powstają lokalnie, przycięte do granicy) i obraz satelitarny EUMETSAT.

use std::sync::atomic::{AtomicBool, AtomicU16, Ordering};
use std::sync::{Arc, Mutex, OnceLock, RwLock};

use serde_json::Value;

use crate::geo::{x_to_lon, y_to_lat, TileId};
use crate::national::{get_bytes, in_poland, merc_bounds, tile_in_poland};

/// Indeksy w kolejce kafli: pola pogody od `FIELD_BASE`, satelita od `SAT_BASE`.
pub const FIELD_BASE: u16 = 50_000;
pub const SAT_BASE: u16 = 59_000;
const LON0: f64 = 14.0;
const LAT0: f64 = 49.0;
const STEP: f64 = 0.5;
const NX: usize = 21;
const NY: usize = 12;
/// Odświeżanie siatki (każdy punkt liczy się jako zapytanie do limitu dziennego).
const REFRESH_SECS: u64 = 3 * 3600;
const MAX_HOURS: usize = 120;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Field {
    Temp,
    Rain,
    Cloud,
    Wind,
}

impl Field {
    pub const ALL: [Field; 4] = [Field::Temp, Field::Rain, Field::Cloud, Field::Wind];

    pub fn label(self) -> &'static str {
        match self {
            Field::Temp => "Temperatura",
            Field::Rain => "Opad",
            Field::Cloud => "Zachmurzenie",
            Field::Wind => "Wiatr",
        }
    }

    pub fn unit(self) -> &'static str {
        match self {
            Field::Temp => "°C",
            Field::Rain => "mm/h",
            Field::Cloud => "%",
            Field::Wind => "m/s",
        }
    }

    fn index(self) -> usize {
        match self {
            Field::Temp => 0,
            Field::Rain => 1,
            Field::Cloud => 2,
            Field::Wind => 3,
        }
    }

    /// Progi barw: (wartość, RGBA); między nimi przejście liniowe.
    pub fn ramp(self) -> &'static [(f32, [u8; 4])] {
        match self {
            Field::Temp => &[(-20.0, [70, 40, 170, 255]), (-5.0, [70, 130, 235, 255]), (2.0, [120, 215, 230, 255]), (12.0, [120, 205, 100, 255]), (22.0, [245, 205, 60, 255]), (35.0, [205, 40, 40, 255])],
            Field::Rain => &[(0.05, [80, 150, 240, 0]), (0.3, [80, 150, 240, 170]), (2.0, [40, 90, 225, 225]), (6.0, [150, 50, 200, 245]), (15.0, [225, 40, 90, 255])],
            Field::Cloud => &[(10.0, [235, 238, 245, 0]), (50.0, [235, 238, 245, 110]), (100.0, [225, 228, 235, 235])],
            Field::Wind => &[(0.0, [120, 205, 150, 40]), (5.0, [120, 205, 100, 170]), (10.0, [245, 205, 60, 215]), (17.0, [235, 110, 45, 240]), (28.0, [170, 30, 110, 255])],
        }
    }
}

fn color_of(f: Field, v: f32) -> [u8; 4] {
    let r = f.ramp();
    if v <= r[0].0 {
        return r[0].1;
    }
    for w in r.windows(2) {
        if v <= w[1].0 {
            let t = (v - w[0].0) / (w[1].0 - w[0].0);
            return std::array::from_fn(|i| (w[0].1[i] as f32 + (w[1].1[i] as f32 - w[0].1[i] as f32) * t) as u8);
        }
    }
    r[r.len() - 1].1
}

pub struct Grid {
    pub times: Vec<i64>,
    /// Pola: [temperatura, opad, zachmurzenie, wiatr], w każdym [godzina][punkt].
    fields: [Vec<Vec<f32>>; 4],
    /// Składowe wiatru (m/s) na wschód i północ.
    u: Vec<Vec<f32>>,
    v: Vec<Vec<f32>>,
    generation: u16,
}

fn slot() -> &'static RwLock<Option<Arc<Grid>>> {
    static G: OnceLock<RwLock<Option<Arc<Grid>>>> = OnceLock::new();
    G.get_or_init(Default::default)
}

pub fn grid() -> Option<Arc<Grid>> {
    slot().read().unwrap().clone()
}

static BUSY: AtomicBool = AtomicBool::new(false);
static GENERATION: AtomicU16 = AtomicU16::new(0);

pub fn busy() -> bool {
    BUSY.load(Ordering::Relaxed)
}

fn last_error() -> &'static Mutex<Option<String>> {
    static E: OnceLock<Mutex<Option<String>>> = OnceLock::new();
    E.get_or_init(Default::default)
}

pub fn error() -> Option<String> {
    last_error().lock().unwrap().clone()
}

/// Pobranie siatki, gdy jej nie ma albo się zestarzała.
pub fn ensure(ctx: &eframe::egui::Context) {
    static LAST: Mutex<Option<std::time::Instant>> = Mutex::new(None);
    {
        let mut l = LAST.lock().unwrap();
        if busy() || l.is_some_and(|t| t.elapsed().as_secs() < REFRESH_SECS) {
            return;
        }
        *l = Some(std::time::Instant::now());
    }
    BUSY.store(true, Ordering::Relaxed);
    let c = ctx.clone();
    std::thread::spawn(move || {
        match fetch_grid() {
            Ok(g) => {
                *slot().write().unwrap() = Some(Arc::new(g));
                *last_error().lock().unwrap() = None;
            }
            Err(e) => {
                log::warn!("pogoda: {e}");
                *last_error().lock().unwrap() = Some(e);
            }
        }
        BUSY.store(false, Ordering::Relaxed);
        c.request_repaint();
    });
}

fn fetch_grid() -> Result<Grid, String> {
    let (mut lats, mut lons) = (Vec::new(), Vec::new());
    for y in 0..NY {
        for x in 0..NX {
            lats.push(format!("{:.1}", LAT0 + y as f64 * STEP));
            lons.push(format!("{:.1}", LON0 + x as f64 * STEP));
        }
    }
    let url = format!(
        "https://api.open-meteo.com/v1/forecast?latitude={}&longitude={}&hourly=temperature_2m,precipitation,cloud_cover,wind_speed_10m,wind_direction_10m&past_days=2&forecast_days=3&timezone=UTC&timeformat=unixtime&wind_speed_unit=ms",
        lats.join(","),
        lons.join(",")
    );
    let b = get_bytes(&url, 64 << 20).map_err(|e| e.to_string())?;
    let j: Value = serde_json::from_slice(&b).map_err(|_| "usługa nie zwróciła danych JSON".to_string())?;
    parse_grid(&j)
}

fn parse_grid(j: &Value) -> Result<Grid, String> {
    let list = j.as_array().filter(|a| a.len() == NX * NY).ok_or("nieoczekiwana odpowiedź usługi pogody")?;
    let times: Vec<i64> = list[0]["hourly"]["time"].as_array().map_or(&[][..], |a| a).iter().filter_map(Value::as_i64).take(MAX_HOURS).collect();
    if times.is_empty() {
        return Err("brak godzin w odpowiedzi".into());
    }
    let n = NX * NY;
    let blank = || vec![vec![0f32; n]; times.len()];
    let (mut fields, mut u, mut v) = ([blank(), blank(), blank(), blank()], blank(), blank());
    for (p, r) in list.iter().enumerate() {
        let h = &r["hourly"];
        let read = |k: &str, t: usize| h[k][t].as_f64().unwrap_or(0.0) as f32;
        for t in 0..times.len() {
            fields[0][t][p] = read("temperature_2m", t);
            fields[1][t][p] = read("precipitation", t);
            fields[2][t][p] = read("cloud_cover", t);
            let speed = read("wind_speed_10m", t);
            fields[3][t][p] = speed;
            // kierunek meteorologiczny: skąd wieje
            let dir = read("wind_direction_10m", t).to_radians();
            u[t][p] = -speed * dir.sin();
            v[t][p] = -speed * dir.cos();
        }
    }
    let generation = GENERATION.fetch_add(1, Ordering::Relaxed) % 18;
    Ok(Grid { times, fields, u, v, generation })
}

impl Grid {
    fn bilinear(data: &[f32], lon: f64, lat: f64) -> f32 {
        let gx = ((lon - LON0) / STEP).clamp(0.0, (NX - 1) as f64);
        let gy = ((lat - LAT0) / STEP).clamp(0.0, (NY - 1) as f64);
        let (x, y) = ((gx as usize).min(NX - 2), (gy as usize).min(NY - 2));
        let (fx, fy) = ((gx - x as f64) as f32, (gy - y as f64) as f32);
        let at = |i: usize, j: usize| data[j * NX + i];
        (at(x, y) * (1.0 - fx) + at(x + 1, y) * fx) * (1.0 - fy) + (at(x, y + 1) * (1.0 - fx) + at(x + 1, y + 1) * fx) * fy
    }

    pub fn value(&self, f: Field, hour: usize, lon: f64, lat: f64) -> Option<f32> {
        Some(Self::bilinear(self.fields[f.index()].get(hour)?, lon, lat))
    }

    pub fn wind(&self, hour: usize, lon: f64, lat: f64) -> Option<(f32, f32)> {
        Some((Self::bilinear(self.u.get(hour)?, lon, lat), Self::bilinear(self.v.get(hour)?, lon, lat)))
    }

    /// Godzina najbliższa chwili obecnej.
    pub fn now_index(&self) -> usize {
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64);
        self.times.iter().enumerate().min_by_key(|(_, t)| (**t - now).abs()).map_or(0, |x| x.0)
    }

    /// Indeks pola w kolejce kafli: nowa siatka, pole lub godzina to nowe kafle.
    pub fn overlay(&self, f: Field, hour: usize) -> u16 {
        FIELD_BASE + (self.generation as usize * 480 + f.index() * MAX_HOURS + hour.min(MAX_HOURS - 1)) as u16
    }
}

/// Czas polski: „04.11 13:00”.
pub fn time_label(unix: i64) -> String {
    let local = unix + crate::sun::warsaw_offset_min(unix as f64) as i64 * 60;
    let d = crate::sun::civil_from_days(local.div_euclid(86_400));
    let s = local.rem_euclid(86_400);
    format!("{:02}.{:02} {:02}:{:02}", d.d, d.m, s / 3600, (s % 3600) / 60)
}

/// Poza Polską przezroczysto (siatka co 8 px).
fn clip(img: &mut image::RgbaImage, t: TileId) {
    let n = (1u64 << t.z) as f64;
    let size = img.width();
    let mut y = 0;
    while y < size {
        let mut x = 0;
        while x < size {
            let lon = x_to_lon((t.x as f64 + (x as f64 + 4.0) / size as f64) / n);
            let lat = y_to_lat((t.y as f64 + (y as f64 + 4.0) / size as f64) / n);
            if !in_poland(lon, lat) {
                for j in y..(y + 8).min(size) {
                    for i in x..(x + 8).min(size) {
                        img.get_pixel_mut(i, j)[3] = 0;
                    }
                }
            }
            x += 8;
        }
        y += 8;
    }
}

/// Kafel pola pogody albo satelity, zależnie od indeksu.
pub fn render_tile(i: u16, t: TileId) -> Option<image::RgbaImage> {
    if !tile_in_poland(t) {
        return None;
    }
    if i >= SAT_BASE {
        return satellite_tile(t);
    }
    let g = grid()?;
    let raw = (i - FIELD_BASE) as usize;
    if raw / 480 != g.generation as usize {
        return None;
    }
    let rest = raw % 480;
    let f = Field::ALL[rest / MAX_HOURS];
    let data = g.fields[f.index()].get(rest % MAX_HOURS)?;
    const N: u32 = crate::layers::TILE_PX;
    let n = (1u64 << t.z) as f64;
    let mut img = image::RgbaImage::new(N, N);
    for y in 0..N {
        let lat = y_to_lat((t.y as f64 + (y as f64 + 0.5) / N as f64) / n);
        for x in 0..N {
            let lon = x_to_lon((t.x as f64 + (x as f64 + 0.5) / N as f64) / n);
            img.put_pixel(x, y, image::Rgba(color_of(f, Grid::bilinear(data, lon, lat))));
        }
    }
    clip(&mut img, t);
    img.pixels().any(|p| p[3] > 0).then_some(img)
}

/// Indeks satelity: zmienia się co 15 minut, żeby kafle wczytały się od nowa.
pub fn satellite_overlay() -> u16 {
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
    SAT_BASE + ((now / 900) % 900) as u16
}

fn satellite_tile(t: TileId) -> Option<image::RgbaImage> {
    let (x0, y0, x1, y1) = merc_bounds(t);
    let url = format!(
        "https://view.eumetsat.int/geoserver/wms?SERVICE=WMS&REQUEST=GetMap&VERSION=1.3.0&LAYERS=mtg_fd:ir105_hrfi&STYLES=&CRS=EPSG:3857&BBOX={x0:.2},{y0:.2},{x1:.2},{y1:.2}&WIDTH={p}&HEIGHT={p}&FORMAT=image/png&TRANSPARENT=TRUE",
        p = crate::layers::TILE_PX
    );
    let b = get_bytes(&url, 8 << 20).ok()?;
    let mut img = image::load_from_memory(&b).ok()?.to_rgba8();
    clip(&mut img, t);
    Some(img)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Grid {
        let n = NX * NY;
        let mut temp = vec![0f32; n];
        for y in 0..NY {
            for x in 0..NX {
                temp[y * NX + x] = x as f32;
            }
        }
        let flat = vec![vec![1f32; n]];
        Grid { times: vec![0], fields: [vec![temp], flat.clone(), flat.clone(), flat.clone()], u: flat.clone(), v: flat, generation: 3 }
    }

    #[test]
    fn bilinear_between_points() {
        let g = sample();
        assert!((g.value(Field::Temp, 0, LON0 + STEP * 2.5, 50.0).unwrap() - 2.5).abs() < 1e-4);
        assert!(g.value(Field::Temp, 5, 20.0, 50.0).is_none());
        // poza siatką: wartość brzegowa, bez paniki
        assert_eq!(g.value(Field::Temp, 0, 40.0, 70.0).unwrap(), (NX - 1) as f32);
    }

    #[test]
    fn overlay_ids_stay_in_range() {
        let g = Grid { generation: 17, ..sample() };
        let top = g.overlay(Field::Wind, MAX_HOURS - 1);
        assert!(top >= FIELD_BASE && top < SAT_BASE, "{top}");
    }

    #[test]
    fn dry_is_transparent() {
        assert_eq!(color_of(Field::Rain, 0.0)[3], 0);
        assert!(color_of(Field::Rain, 3.0)[3] > 200);
    }

    #[test]
    fn parses_response() {
        let one = serde_json::json!({"hourly":{"time":[100,200],"temperature_2m":[1.0,2.0],"precipitation":[0.0,0.5],"cloud_cover":[10,20],"wind_speed_10m":[5.0,6.0],"wind_direction_10m":[270.0,0.0]}});
        let j = Value::Array(vec![one; NX * NY]);
        let g = parse_grid(&j).unwrap();
        assert_eq!(g.times, vec![100, 200]);
        assert_eq!(g.value(Field::Temp, 1, 20.0, 52.0).unwrap(), 2.0);
        // wiatr z zachodu wieje na wschód
        assert!(g.wind(0, 20.0, 52.0).unwrap().0 > 4.9);
        assert!(parse_grid(&serde_json::json!({"error":true})).is_err());
    }
}
