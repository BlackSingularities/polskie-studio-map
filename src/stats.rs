//! Statystyka: wskaźniki z API GUS BDL złączone z granicami PRG (`assets/units.json`) po kodzie TERYT.
//! Kartogram jest rysowany jak warstwa: kafle powstają lokalnie z wielokątów.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock, RwLock};

use serde_json::Value;

use crate::feeds::get_json;
use crate::geo::{y_to_lat, TileId};
use crate::national::in_ring;
use crate::osm::urlencode;

const API: &str = "https://bdl.stat.gov.pl/api/v1";
/// Numeracja warstw kartogramu w kolejce kafli: nowa wersja danych to nowe kafle.
pub const OVERLAY_BASE: u16 = 60_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Level {
    Voivodeship,
    County,
    Commune,
}

impl Level {
    pub const ALL: [Level; 3] = [Level::Voivodeship, Level::County, Level::Commune];

    pub fn label(self) -> &'static str {
        match self {
            Level::Voivodeship => "województwa",
            Level::County => "powiaty",
            Level::Commune => "gminy",
        }
    }

    fn api(self) -> u8 {
        match self {
            Level::Voivodeship => 2,
            Level::County => 5,
            Level::Commune => 6,
        }
    }

    fn key(self) -> &'static str {
        match self {
            Level::Voivodeship => "woj",
            Level::County => "pow",
            Level::Commune => "gm",
        }
    }

    /// Identyfikator jednostki BDL → kod TERYT: `011212001011` → `1201011`.
    fn teryt(self, id: &str) -> Option<String> {
        if id.len() != 12 {
            return None;
        }
        let (w, p, g) = (&id[2..4], &id[7..9], &id[9..12]);
        Some(match self {
            Level::Voivodeship => w.to_string(),
            Level::County => format!("{w}{p}"),
            Level::Commune => format!("{w}{p}{g}"),
        })
    }
}

pub struct Unit {
    pub teryt: String,
    pub name: String,
    rings: Vec<Vec<[f64; 2]>>,
    /// zachód, południe, wschód, północ
    bbox: [f64; 4],
    pub center: [f64; 2],
}

fn all_units() -> &'static HashMap<&'static str, Vec<Unit>> {
    static U: OnceLock<HashMap<&'static str, Vec<Unit>>> = OnceLock::new();
    U.get_or_init(|| {
        let j: Value = serde_json::from_str(include_str!("../assets/units.json")).expect("granice jednostek");
        let mut out = HashMap::new();
        for level in Level::ALL {
            let list = j[level.key()].as_array().map_or(&[][..], |a| a).iter().map(parse_unit).collect();
            out.insert(level.key(), list);
        }
        out
    })
}

fn parse_unit(u: &Value) -> Unit {
    let rings: Vec<Vec<[f64; 2]>> = u["r"]
        .as_array()
        .map_or(&[][..], |a| a)
        .iter()
        .map(|r| {
            let n: Vec<f64> = r.as_array().map_or(&[][..], |a| a).iter().filter_map(Value::as_f64).map(|v| v / 10_000.0).collect();
            n.chunks_exact(2).map(|p| [p[0], p[1]]).collect()
        })
        .collect();
    let mut bbox = [f64::MAX, f64::MAX, f64::MIN, f64::MIN];
    for p in rings.iter().flatten() {
        bbox = [bbox[0].min(p[0]), bbox[1].min(p[1]), bbox[2].max(p[0]), bbox[3].max(p[1])];
    }
    // środek największego pierścienia (średnia wierzchołków)
    let big = rings.iter().max_by_key(|r| r.len());
    let center = big.map_or([0.0, 0.0], |r| {
        let n = r.len().max(1) as f64;
        [r.iter().map(|p| p[0]).sum::<f64>() / n, r.iter().map(|p| p[1]).sum::<f64>() / n]
    });
    Unit {
        teryt: u["t"].as_str().unwrap_or("").to_string(),
        name: u["n"].as_str().unwrap_or("").to_string(),
        rings,
        bbox,
        center,
    }
}

pub fn units(level: Level) -> &'static [Unit] {
    &all_units()[level.key()]
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Method {
    Quantiles,
    NaturalBreaks,
}

pub const CLASSES: usize = 5;
pub const PALETTE: [[u8; 3]; CLASSES] = [[255, 245, 200], [254, 204, 110], [250, 150, 60], [224, 80, 50], [150, 25, 50]];

/// Dolne granice klas 2..5 z wartości.
pub fn classify(values: &[f64], m: Method) -> [f64; CLASSES - 1] {
    let mut v: Vec<f64> = values.iter().copied().filter(|x| x.is_finite()).collect();
    v.sort_by(f64::total_cmp);
    if v.is_empty() {
        return [0.0; CLASSES - 1];
    }
    match m {
        Method::Quantiles => std::array::from_fn(|i| v[(v.len() * (i + 1) / CLASSES).min(v.len() - 1)]),
        Method::NaturalBreaks => jenks(&v),
    }
}

/// Podział Jenksa (programowanie dynamiczne, na próbce do 400 wartości).
fn jenks(sorted: &[f64]) -> [f64; CLASSES - 1] {
    let step = (sorted.len() / 400).max(1);
    let v: Vec<f64> = sorted.iter().step_by(step).copied().collect();
    let n = v.len();
    let (mut s1, mut s2) = (vec![0.0; n + 1], vec![0.0; n + 1]);
    for i in 0..n {
        s1[i + 1] = s1[i] + v[i];
        s2[i + 1] = s2[i] + v[i] * v[i];
    }
    let cost = |a: usize, b: usize| {
        let c = (b - a) as f64;
        s2[b] - s2[a] - (s1[b] - s1[a]).powi(2) / c
    };
    let mut best = vec![vec![f64::INFINITY; n + 1]; CLASSES + 1];
    let mut from = vec![vec![0usize; n + 1]; CLASSES + 1];
    best[0][0] = 0.0;
    for k in 1..=CLASSES {
        for b in 1..=n {
            for a in 0..b {
                let c = best[k - 1][a] + cost(a, b);
                if c < best[k][b] {
                    best[k][b] = c;
                    from[k][b] = a;
                }
            }
        }
    }
    let mut out = [v[n - 1]; CLASSES - 1];
    let mut b = n;
    for k in (1..=CLASSES).rev() {
        let a = from[k][b];
        if k >= 2 {
            out[k - 2] = v[a.min(n - 1)];
        }
        b = a;
    }
    out
}

pub fn class_of(v: f64, breaks: &[f64; CLASSES - 1]) -> usize {
    breaks.iter().filter(|b| v >= **b).count()
}

/// Bieżący kartogram.
pub struct Map {
    pub level: Level,
    pub label: String,
    pub unit: String,
    pub year: i32,
    pub values: HashMap<String, f64>,
    pub breaks: [f64; CLASSES - 1],
    pub version: u16,
}

fn current() -> &'static RwLock<Option<Arc<Map>>> {
    static C: OnceLock<RwLock<Option<Arc<Map>>>> = OnceLock::new();
    C.get_or_init(Default::default)
}

pub fn map() -> Option<Arc<Map>> {
    current().read().unwrap().clone()
}

pub fn clear_map() {
    *current().write().unwrap() = None;
}

/// Indeks warstwy kartogramu w kolejce kafli.
pub fn overlay() -> Option<u16> {
    map().map(|m| OVERLAY_BASE + m.version)
}

/// Zmiana sposobu klasyfikacji: te same dane, nowe kafle.
pub fn reclassify(method: Method) {
    let mut c = current().write().unwrap();
    if let Some(m) = c.as_ref() {
        let vals: Vec<f64> = m.values.values().copied().collect();
        *c = Some(Arc::new(Map {
            level: m.level,
            label: m.label.clone(),
            unit: m.unit.clone(),
            year: m.year,
            breaks: classify(&vals, method),
            values: m.values.clone(),
            version: next_version(),
        }));
    }
}

fn next_version() -> u16 {
    static V: std::sync::atomic::AtomicU16 = std::sync::atomic::AtomicU16::new(0);
    V.fetch_add(1, std::sync::atomic::Ordering::Relaxed) % 5000
}

/// Kafel kartogramu: wielokąty jednostek wypełnione barwą klasy.
pub fn render_tile(overlay: u16, t: TileId) -> Option<image::RgbaImage> {
    let m = map().filter(|m| OVERLAY_BASE + m.version == overlay)?;
    const N: u32 = crate::layers::TILE_PX;
    let b = t.bbox();
    let n = (1u64 << t.z) as f64;
    let mut img = image::RgbaImage::new(N, N);
    let mut any = false;
    for u in units(m.level) {
        if u.bbox[0] > b.e || u.bbox[2] < b.w || u.bbox[1] > b.n || u.bbox[3] < b.s {
            continue;
        }
        let Some(v) = m.values.get(&u.teryt) else { continue };
        let c = PALETTE[class_of(*v, &m.breaks)];
        for y in 0..N {
            let lat = y_to_lat((t.y as f64 + (y as f64 + 0.5) / N as f64) / n);
            if lat < u.bbox[1] || lat > u.bbox[3] {
                continue;
            }
            let mut xs: Vec<f64> = Vec::new();
            for r in &u.rings {
                let mut j = r.len().wrapping_sub(1);
                for i in 0..r.len() {
                    let (p, q) = (r[i], r[j]);
                    if (p[1] > lat) != (q[1] > lat) {
                        xs.push(p[0] + (lat - p[1]) * (q[0] - p[0]) / (q[1] - p[1]));
                    }
                    j = i;
                }
            }
            xs.sort_by(f64::total_cmp);
            for s in xs.chunks_exact(2) {
                let to_px = |lon: f64| ((lon - b.w) / (b.e - b.w) * N as f64).clamp(0.0, N as f64) as u32;
                for x in to_px(s[0])..to_px(s[1]) {
                    img.put_pixel(x, y, image::Rgba([c[0], c[1], c[2], 255]));
                    any = true;
                }
            }
        }
    }
    any.then_some(img)
}

/// Jednostka i wartość w punkcie.
pub fn value_at(lon: f64, lat: f64) -> Option<(String, f64, Arc<Map>)> {
    let m = map()?;
    let u = units(m.level).iter().find(|u| {
        lon >= u.bbox[0] && lon <= u.bbox[2] && lat >= u.bbox[1] && lat <= u.bbox[3] && u.rings.iter().filter(|r| in_ring(lon, lat, r)).count() % 2 == 1
    })?;
    let v = *m.values.get(&u.teryt)?;
    Some((u.name.clone(), v, m))
}

// ---------------------------------------------------------------- API

#[derive(Clone, Debug)]
pub struct Subject {
    pub id: String,
    pub name: String,
    pub has_vars: bool,
}

#[derive(Clone, Debug)]
pub struct Variable {
    pub id: i64,
    pub name: String,
    pub unit: String,
    /// Najniższy poziom z danymi (2, 5, 6…).
    pub level: u8,
}

/// Stan przeglądarki tematów, współdzielony z wątkami pobierania.
#[derive(Default)]
pub struct Browser {
    pub subjects: HashMap<String, Vec<Subject>>,
    pub variables: HashMap<String, Vec<Variable>>,
    pub found: Vec<Variable>,
    pub busy: bool,
    pub progress: Option<(usize, usize)>,
    pub error: Option<String>,
    asked: std::collections::HashSet<String>,
}

pub type Shared = Arc<Mutex<Browser>>;

fn parse_variables(j: &Value) -> Vec<Variable> {
    j["results"]
        .as_array()
        .map_or(&[][..], |a| a)
        .iter()
        .filter_map(|r| {
            let parts: Vec<&str> = (1..=5).filter_map(|i| r[format!("n{i}")].as_str()).collect();
            Some(Variable {
                id: r["id"].as_i64()?,
                name: parts.join(" · "),
                unit: r["measureUnitName"].as_str().unwrap_or("").to_string(),
                level: r["level"].as_u64().unwrap_or(0) as u8,
            })
        })
        .collect()
}

fn in_background(s: &Shared, ctx: &eframe::egui::Context, job: impl FnOnce(&Shared) -> Result<(), String> + Send + 'static) {
    let (s, c) = (s.clone(), ctx.clone());
    std::thread::spawn(move || {
        let r = job(&s);
        s.lock().unwrap().error = r.err();
        c.request_repaint();
    });
}

/// Działy tematyczne: `parent` puste – najwyższy poziom. Pobierane raz.
pub fn ask_subjects(s: &Shared, parent: &str, ctx: &eframe::egui::Context) {
    if !s.lock().unwrap().asked.insert(format!("s{parent}")) {
        return;
    }
    let p = parent.to_string();
    in_background(s, ctx, move |s| {
        let q = if p.is_empty() { String::new() } else { format!("&parent-id={p}") };
        let j = get_json(&format!("{API}/subjects?format=json&lang=pl&page-size=100{q}"))?;
        let list = j["results"]
            .as_array()
            .map_or(&[][..], |a| a)
            .iter()
            .map(|r| Subject {
                id: r["id"].as_str().unwrap_or("").to_string(),
                name: r["name"].as_str().unwrap_or("").to_string(),
                has_vars: r["hasVariables"].as_bool().unwrap_or(false),
            })
            .collect();
        s.lock().unwrap().subjects.insert(p, list);
        Ok(())
    });
}

pub fn ask_variables(s: &Shared, subject: &str, ctx: &eframe::egui::Context) {
    if !s.lock().unwrap().asked.insert(format!("v{subject}")) {
        return;
    }
    let p = subject.to_string();
    in_background(s, ctx, move |s| {
        let j = get_json(&format!("{API}/variables?subject-id={p}&format=json&lang=pl&page-size=100"))?;
        s.lock().unwrap().variables.insert(p, parse_variables(&j));
        Ok(())
    });
}

pub fn search(s: &Shared, q: &str, ctx: &eframe::egui::Context) {
    let q = q.to_string();
    in_background(s, ctx, move |s| {
        let j = get_json(&format!("{API}/variables/search?name={}&format=json&lang=pl&page-size=50", urlencode(&q)))?;
        s.lock().unwrap().found = parse_variables(&j);
        Ok(())
    });
}

/// Pobranie wskaźnika i ustawienie kartogramu. Stronami, spokojnie (limit anonimowy API).
pub fn load(s: &Shared, v: Variable, level: Level, year: i32, method: Method, ctx: &eframe::egui::Context) {
    {
        let mut g = s.lock().unwrap();
        if g.busy {
            return;
        }
        g.busy = true;
        g.progress = None;
    }
    let c = ctx.clone();
    in_background(s, ctx, move |s| {
        let r = fetch_values(s, v.id, level, year, &c);
        let mut g = s.lock().unwrap();
        g.busy = false;
        g.progress = None;
        drop(g);
        let values = r?;
        if values.is_empty() {
            return Err(format!("Brak danych: {} za {year}. Spróbuj inny rok albo poziom.", level.label()));
        }
        let vals: Vec<f64> = values.values().copied().collect();
        *current().write().unwrap() = Some(Arc::new(Map {
            level,
            label: v.name,
            unit: v.unit,
            year,
            breaks: classify(&vals, method),
            values,
            version: next_version(),
        }));
        Ok(())
    });
}

fn fetch_values(s: &Shared, var: i64, level: Level, year: i32, ctx: &eframe::egui::Context) -> Result<HashMap<String, f64>, String> {
    let mut out = HashMap::new();
    let mut page = 0usize;
    loop {
        let j = get_json(&format!("{API}/data/by-variable/{var}?format=json&unit-level={}&year={year}&page-size=100&page={page}", level.api()))?;
        out.extend(parse_values(&j, level));
        let total = j["totalRecords"].as_u64().unwrap_or(0) as usize;
        page += 1;
        s.lock().unwrap().progress = Some(((page * 100).min(total), total));
        ctx.request_repaint();
        if page * 100 >= total {
            return Ok(out);
        }
        std::thread::sleep(std::time::Duration::from_millis(250));
    }
}

fn parse_values(j: &Value, level: Level) -> Vec<(String, f64)> {
    j["results"]
        .as_array()
        .map_or(&[][..], |a| a)
        .iter()
        .filter_map(|r| {
            let t = level.teryt(r["id"].as_str()?)?;
            let v = r["values"].as_array()?.first()?["val"].as_f64()?;
            Some((t, v))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn teryt_from_bdl_id() {
        assert_eq!(Level::Commune.teryt("011212001011").unwrap(), "1201011");
        assert_eq!(Level::County.teryt("011212006000").unwrap(), "1206");
        assert_eq!(Level::Voivodeship.teryt("012400000000").unwrap(), "24");
    }

    #[test]
    fn units_match_expected_counts() {
        assert_eq!(units(Level::Voivodeship).len(), 16);
        assert_eq!(units(Level::County).len(), 380);
        assert!(units(Level::Commune).len() > 2400);
        assert!(units(Level::County).iter().any(|u| u.teryt == "1201"));
        assert!(units(Level::Commune).iter().any(|u| u.teryt == "1201011"));
    }

    #[test]
    fn classes_split_values() {
        let v: Vec<f64> = (0..100).map(|x| x as f64).collect();
        let q = classify(&v, Method::Quantiles);
        assert_eq!(q, [20.0, 40.0, 60.0, 80.0]);
        assert_eq!(class_of(5.0, &q), 0);
        assert_eq!(class_of(99.0, &q), 4);
        let mut two = vec![1.0; 50];
        two.extend(vec![100.0; 50]);
        two.extend(vec![200.0; 50]);
        let j = classify(&two, Method::NaturalBreaks);
        assert!(j.windows(2).all(|w| w[0] <= w[1]));
        assert_eq!(class_of(200.0, &j) > class_of(1.0, &j), true);
    }

    #[test]
    fn parses_values() {
        let j = serde_json::json!({"results":[{"id":"011212001000","name":"x","values":[{"year":"2022","val":164.7}]},{"id":"bad","values":[{"val":1.0}]}]});
        assert_eq!(parse_values(&j, Level::County), vec![("1201".to_string(), 164.7)]);
    }
}
