//! Dane na żywo: punkty z usług (IMGW, GIOŚ, sensor.community, Nextbike, OpenSky) i ostrzeżenia,
//! odświeżane w tle. Historia wartości narasta od uruchomienia programu – usługi jej nie podają.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::Value;

use crate::national::{get_bytes, in_poland};
use crate::query::Pairs;

#[derive(Clone, Debug)]
pub struct Point {
    pub key: String,
    pub name: String,
    pub lon: f64,
    pub lat: f64,
    pub value: f32,
    /// Tekst przy punkcie i w panelu.
    pub label: String,
    pub color: [u8; 3],
    pub radius: f32,
    pub details: Pairs,
}

pub struct FeedDef {
    pub id: &'static str,
    pub name: &'static str,
    pub attribution: &'static str,
    refresh: u64,
    fetch: fn() -> Result<Fetched, String>,
}

#[derive(Default)]
pub struct Fetched {
    pub points: Vec<Point>,
    /// (tytuł, treść)
    pub warnings: Vec<(String, String)>,
}

pub const FEEDS: [FeedDef; 7] = [
    FeedDef { id: "imgw.hydro", name: "Stany wód (IMGW)", attribution: "Źródłem danych jest IMGW-PIB", refresh: 1800, fetch: fetch_hydro },
    FeedDef { id: "imgw.meteo", name: "Temperatura i wiatr (IMGW)", attribution: "Źródłem danych jest IMGW-PIB", refresh: 1800, fetch: fetch_meteo },
    FeedDef { id: "imgw.ostrzezenia", name: "Ostrzeżenia IMGW", attribution: "Źródłem danych jest IMGW-PIB", refresh: 900, fetch: fetch_warnings },
    FeedDef { id: "gios.indeks", name: "Jakość powietrza (GIOŚ)", attribution: "© GIOŚ", refresh: 3600, fetch: fetch_gios },
    FeedDef { id: "sensor.pm25", name: "Smog PM2.5 (sensor.community)", attribution: "sensor.community", refresh: 900, fetch: fetch_sensors },
    FeedDef { id: "nextbike", name: "Rowery miejskie (Nextbike)", attribution: "Nextbike", refresh: 300, fetch: fetch_bikes },
    FeedDef { id: "opensky", name: "Samoloty (OpenSky)", attribution: "OpenSky Network, użytek niekomercyjny", refresh: 600, fetch: fetch_planes },
];

#[derive(Default)]
struct State {
    points: Arc<Vec<Point>>,
    warnings: Arc<Vec<(String, String)>>,
    updated: Option<Instant>,
    attempt: Option<Instant>,
    busy: bool,
    error: Option<String>,
    history: HashMap<String, VecDeque<f32>>,
}

/// Stan feedu dla panelu.
pub struct Status {
    pub count: usize,
    pub busy: bool,
    pub error: Option<String>,
    pub age_min: Option<u64>,
}

#[derive(Clone, Default)]
pub struct Hub {
    state: Arc<Mutex<HashMap<&'static str, State>>>,
}

const HISTORY: usize = 96;

impl Hub {
    /// Co klatkę: uruchamia w tle pobieranie włączonych feedów, którym minął czas odświeżenia.
    pub fn sync(&self, enabled: &[String], ctx: &eframe::egui::Context) {
        for def in &FEEDS {
            if !enabled.iter().any(|e| e == def.id) {
                continue;
            }
            let mut map = self.state.lock().unwrap();
            let s = map.entry(def.id).or_default();
            if s.busy || s.attempt.is_some_and(|t| t.elapsed() < Duration::from_secs(def.refresh)) {
                continue;
            }
            s.busy = true;
            s.attempt = Some(Instant::now());
            drop(map);
            let (hub, c, id, fetch) = (self.clone(), ctx.clone(), def.id, def.fetch);
            std::thread::spawn(move || {
                let r = fetch();
                let mut map = hub.state.lock().unwrap();
                let s = map.entry(id).or_default();
                s.busy = false;
                match r {
                    Ok(f) => {
                        for p in &f.points {
                            let h = s.history.entry(p.key.clone()).or_default();
                            h.push_back(p.value);
                            if h.len() > HISTORY {
                                h.pop_front();
                            }
                        }
                        s.points = Arc::new(f.points);
                        s.warnings = Arc::new(f.warnings);
                        s.updated = Some(Instant::now());
                        s.error = None;
                    }
                    Err(e) => {
                        log::warn!("dane na żywo {id}: {e}");
                        s.error = Some(e);
                    }
                }
                c.request_repaint();
            });
        }
    }

    pub fn points(&self, id: &str) -> Arc<Vec<Point>> {
        self.state.lock().unwrap().get(id).map(|s| s.points.clone()).unwrap_or_default()
    }

    pub fn warnings(&self, id: &str) -> Arc<Vec<(String, String)>> {
        self.state.lock().unwrap().get(id).map(|s| s.warnings.clone()).unwrap_or_default()
    }

    pub fn history(&self, id: &str, key: &str) -> Vec<f32> {
        self.state.lock().unwrap().get(id).and_then(|s| s.history.get(key)).map(|h| h.iter().copied().collect()).unwrap_or_default()
    }

    pub fn status(&self, id: &str) -> Status {
        let map = self.state.lock().unwrap();
        match map.get(id) {
            Some(s) => Status {
                count: s.points.len() + s.warnings.len(),
                busy: s.busy,
                error: s.error.clone(),
                age_min: s.updated.map(|t| t.elapsed().as_secs() / 60),
            },
            None => Status { count: 0, busy: false, error: None, age_min: None },
        }
    }
}

pub fn get_json(url: &str) -> Result<Value, String> {
    let b = get_bytes(url, 32 << 20).map_err(|e| e.to_string())?;
    serde_json::from_slice(&b).map_err(|_| "usługa nie zwróciła danych JSON".to_string())
}

/// Liczba podana jako liczba albo tekst.
fn num(v: &Value) -> Option<f64> {
    v.as_f64().or_else(|| v.as_str()?.trim().parse().ok()).filter(|x: &f64| x.is_finite())
}

fn text(v: &Value) -> String {
    v.as_str().map(|s| s.trim().to_string()).unwrap_or_else(|| if v.is_null() { String::new() } else { v.to_string() })
}

/// Wszystkie niepuste pola rekordu poza technicznymi, z czytelnymi nazwami.
fn all_fields(r: &Value, skip: &[&str]) -> Pairs {
    let Some(o) = r.as_object() else { return Vec::new() };
    o.iter()
        .filter(|(k, v)| !v.is_null() && !skip.contains(&k.as_str()))
        .map(|(k, v)| (k.replace('_', " "), text(v)))
        .collect()
}

fn hydro_points(j: &Value) -> Vec<Point> {
    let mut out = Vec::new();
    for r in j.as_array().map_or(&[][..], |a| a) {
        let (Some(lon), Some(lat), Some(level)) = (num(&r["lon"]), num(&r["lat"]), num(&r["stan_wody"])) else { continue };
        if !in_poland(lon, lat) {
            continue;
        }
        let (warn, alarm) = (num(&r["stan_ostrzegawczy"]), num(&r["stan_alarmowy"]));
        let (color, radius, rank) = if alarm.is_some_and(|a| level >= a) {
            ([226, 60, 48], 8.0, 2.0)
        } else if warn.is_some_and(|w| level >= w) {
            ([246, 170, 40], 7.0, 1.0)
        } else {
            ([70, 150, 230], 4.5, 0.0)
        };
        out.push((rank, Point {
            key: text(&r["id_stacji"]),
            name: format!("{} ({})", text(&r["stacja"]), text(&r["rzeka"])),
            lon,
            lat,
            value: level as f32,
            label: format!("{level:.0} cm"),
            color,
            radius,
            details: all_fields(r, &["id_stacji", "stacja", "lon", "lat", "rok_zalozenia_stacji"]),
        }));
    }
    ranked(out)
}

/// Najważniejsze na początek: przy zagęszczeniu na ekranie zostają one.
fn ranked(mut v: Vec<(f64, Point)>) -> Vec<Point> {
    v.sort_by(|a, b| b.0.total_cmp(&a.0));
    v.into_iter().map(|x| x.1).collect()
}

fn fetch_hydro() -> Result<Fetched, String> {
    Ok(Fetched { points: hydro_points(&get_json("https://danepubliczne.imgw.pl/api/data/hydro")?), warnings: Vec::new() })
}

fn temp_color(t: f64) -> [u8; 3] {
    const RAMP: [(f64, [u8; 3]); 6] = [(-15.0, [90, 60, 190]), (-5.0, [70, 130, 235]), (2.0, [110, 210, 225]), (12.0, [120, 205, 100]), (22.0, [245, 200, 60]), (32.0, [225, 60, 45])];
    RAMP.iter().rev().find(|(v, _)| t >= *v).map_or(RAMP[0].1, |x| x.1)
}

fn meteo_points(j: &Value) -> Vec<Point> {
    j.as_array().map_or(&[][..], |a| a).iter().filter_map(|r| {
        let (lon, lat, t) = (num(&r["lon"])?, num(&r["lat"])?, num(&r["temperatura_powietrza"])?);
        in_poland(lon, lat).then(|| Point {
            key: text(&r["kod_stacji"]),
            name: text(&r["nazwa_stacji"]),
            lon,
            lat,
            value: t as f32,
            label: format!("{t:.1}°C"),
            color: temp_color(t),
            radius: 6.0,
            details: all_fields(r, &["kod_stacji", "nazwa_stacji", "lon", "lat", "rok_zalozenia_stacji"]),
        })
    }).collect()
}

fn fetch_meteo() -> Result<Fetched, String> {
    Ok(Fetched { points: meteo_points(&get_json("https://danepubliczne.imgw.pl/api/data/meteo")?), warnings: Vec::new() })
}

fn warning_items(j: &Value) -> Vec<(String, String)> {
    j.as_array().map_or(&[][..], |a| a).iter().map(|r| {
        let first = |keys: &[&str]| keys.iter().map(|k| text(&r[*k])).find(|s| !s.is_empty()).unwrap_or_default();
        let areas: Vec<String> = r["obszary"].as_array().map_or(&[][..], |a| a).iter().map(|o| text(&o["opis"])).take(6).collect();
        let mut body = first(&["przebieg", "tresc", "komentarz"]);
        if !areas.is_empty() {
            body = format!("{body}\n{}", areas.join("; "));
        }
        let level = first(&["stopień", "stopien"]);
        (format!("{} · stopień {level}", first(&["zdarzenie", "nazwa_zdarzenia"])), body)
    }).collect()
}

fn fetch_warnings() -> Result<Fetched, String> {
    let mut warnings = warning_items(&get_json("https://danepubliczne.imgw.pl/api/data/warningsmeteo")?);
    warnings.extend(warning_items(&get_json("https://danepubliczne.imgw.pl/api/data/warningshydro")?));
    Ok(Fetched { points: Vec::new(), warnings })
}

fn aqi_color(i: i64) -> [u8; 3] {
    match i {
        0 => [70, 190, 90],
        1 => [150, 205, 80],
        2 => [245, 210, 60],
        3 => [245, 150, 50],
        4 => [225, 60, 45],
        _ => [130, 40, 90],
    }
}

/// Indeks trzeba pobrać dla każdej stacji osobno, więc spokojnie, po kolei.
fn fetch_gios() -> Result<Fetched, String> {
    let list = get_json("https://api.gios.gov.pl/pjp-api/v1/rest/station/findAll?size=500&page=0")?;
    let mut points = Vec::new();
    for r in list["Lista stacji pomiarowych"].as_array().map_or(&[][..], |a| a) {
        let (Some(lon), Some(lat), Some(id)) = (num(&r["WGS84 \u{3bb} E"]), num(&r["WGS84 \u{3c6} N"]), r["Identyfikator stacji"].as_i64()) else { continue };
        std::thread::sleep(Duration::from_millis(150));
        let Ok(j) = get_json(&format!("https://api.gios.gov.pl/pjp-api/v1/rest/aqindex/getIndex/{id}")) else { continue };
        let a = &j["AqIndex"];
        let Some(idx) = a["Warto\u{15b}\u{107} indeksu"].as_i64() else { continue };
        let cat = text(&a["Nazwa kategorii indeksu"]);
        points.push((idx as f64, Point {
            key: id.to_string(),
            name: format!("{}, {}", text(&r["Nazwa miasta"]), text(&r["Ulica"])),
            lon,
            lat,
            value: idx as f32,
            label: cat.clone(),
            color: aqi_color(idx),
            radius: 6.0 + idx as f32,
            details: vec![("Indeks jakości powietrza".into(), cat), ("Czas".into(), text(&a["Data wykonania oblicze\u{144} indeksu"]))],
        }));
    }
    if points.is_empty() {
        return Err("brak odpowiedzi stacji".into());
    }
    Ok(Fetched { points: ranked(points), warnings: Vec::new() })
}

fn sensor_points(j: &Value) -> Vec<Point> {
    let mut by_place: HashMap<i64, (f64, Point)> = HashMap::new();
    for r in j.as_array().map_or(&[][..], |a| a) {
        let l = &r["location"];
        if l["indoor"].as_i64() == Some(1) {
            continue;
        }
        let (Some(lon), Some(lat), Some(id)) = (num(&l["longitude"]), num(&l["latitude"]), l["id"].as_i64()) else { continue };
        let vals = r["sensordatavalues"].as_array().map_or(&[][..], |a| a);
        let Some(pm) = vals.iter().find(|v| v["value_type"] == "P2").and_then(|v| num(&v["value"])) else { continue };
        if !in_poland(lon, lat) || !(0.0..1000.0).contains(&pm) {
            continue;
        }
        let color = match pm {
            x if x < 13.0 => [70, 190, 90],
            x if x < 35.0 => [150, 205, 80],
            x if x < 55.0 => [245, 210, 60],
            x if x < 75.0 => [245, 150, 50],
            x if x < 110.0 => [225, 60, 45],
            _ => [130, 40, 90],
        };
        by_place.insert(id, (pm, Point {
            key: id.to_string(),
            name: format!("Czujnik {id}"),
            lon,
            lat,
            value: pm as f32,
            label: format!("{pm:.0} µg/m³"),
            color,
            radius: (4.0 + pm as f32 / 20.0).min(10.0),
            details: vec![("PM2.5".into(), format!("{pm:.1} µg/m³")), ("Czas (UTC)".into(), text(&r["timestamp"]))],
        }));
    }
    ranked(by_place.into_values().collect())
}

fn fetch_sensors() -> Result<Fetched, String> {
    Ok(Fetched { points: sensor_points(&get_json("https://data.sensor.community/airrohr/v1/filter/country=PL")?), warnings: Vec::new() })
}

fn bike_points(info: &Value, status: &Value) -> Vec<Point> {
    let free: HashMap<String, &Value> = status["data"]["stations"].as_array().map_or(&[][..], |a| a).iter().map(|s| (text(&s["station_id"]), s)).collect();
    info["data"]["stations"].as_array().map_or(&[][..], |a| a).iter().filter_map(|r| {
        let (lon, lat) = (num(&r["lon"])?, num(&r["lat"])?);
        let id = text(&r["station_id"]);
        let bikes = num(&free.get(&id)?["num_bikes_available"])?;
        in_poland(lon, lat).then(|| Point {
            name: text(&r["name"]),
            lon,
            lat,
            value: bikes as f32,
            label: format!("{bikes:.0}"),
            color: if bikes > 0.0 { [60, 170, 110] } else { [150, 150, 150] },
            radius: (3.5 + bikes as f32 * 0.4).min(9.0),
            details: vec![("Dostępne rowery".into(), format!("{bikes:.0}")), ("Pojemność".into(), text(&r["capacity"]))],
            key: id,
        })
    }).collect()
}

fn fetch_bikes() -> Result<Fetched, String> {
    const B: &str = "https://gbfs.nextbike.net/maps/gbfs/v2/nextbike_pl/pl";
    let (i, s) = (get_json(&format!("{B}/station_information.json"))?, get_json(&format!("{B}/station_status.json"))?);
    Ok(Fetched { points: bike_points(&i, &s), warnings: Vec::new() })
}

fn plane_points(j: &Value) -> Vec<Point> {
    j["states"].as_array().map_or(&[][..], |a| a).iter().filter_map(|r| {
        let (lon, lat) = (num(&r[5])?, num(&r[6])?);
        if !in_poland(lon, lat) || r[8].as_bool() == Some(true) {
            return None;
        }
        let alt = num(&r[7]).unwrap_or(0.0);
        let call = text(&r[1]);
        Some(Point {
            key: text(&r[0]),
            name: if call.is_empty() { text(&r[0]) } else { call.clone() },
            lon,
            lat,
            value: alt as f32,
            label: call,
            color: [240, 240, 250],
            radius: 4.5,
            details: vec![
                ("Kraj rejestracji".into(), text(&r[2])),
                ("Wysokość".into(), format!("{alt:.0} m")),
                ("Prędkość".into(), format!("{:.0} km/h", num(&r[9]).unwrap_or(0.0) * 3.6)),
                ("Kurs".into(), format!("{:.0}°", num(&r[10]).unwrap_or(0.0))),
            ],
        })
    }).collect()
}

fn fetch_planes() -> Result<Fetched, String> {
    Ok(Fetched { points: plane_points(&get_json("https://opensky-network.org/api/states/all?lamin=49&lomin=14&lamax=54.9&lomax=24.2")?), warnings: Vec::new() })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn hydro_flags_alarm_and_drops_foreign() {
        let j = json!([
            {"id_stacji":"1","stacja":"A","rzeka":"R","lon":"19.9","lat":"50.0","stan_wody":"350","stan_ostrzegawczy":"300","stan_alarmowy":"340"},
            {"id_stacji":"2","stacja":"B","rzeka":"R","lon":"20.0","lat":"52.0","stan_wody":"100","stan_ostrzegawczy":"300","stan_alarmowy":"340"},
            {"id_stacji":"3","stacja":"C","rzeka":"R","lon":"30.0","lat":"52.0","stan_wody":"100"},
            {"id_stacji":"4","stacja":"D","rzeka":"R","lon":null,"lat":null,"stan_wody":"100"}]);
        let p = hydro_points(&j);
        assert_eq!(p.len(), 2);
        assert_eq!(p[0].key, "1");
        assert_eq!(p[0].color, [226, 60, 48]);
    }

    #[test]
    fn sensors_skip_indoor() {
        let j = json!([
            {"timestamp":"t","location":{"id":1,"indoor":0,"latitude":"52.0","longitude":"20.0"},"sensordatavalues":[{"value_type":"P2","value":"40.0"}]},
            {"timestamp":"t","location":{"id":2,"indoor":1,"latitude":"52.0","longitude":"20.0"},"sensordatavalues":[{"value_type":"P2","value":"40.0"}]},
            {"timestamp":"t","location":{"id":3,"indoor":0,"latitude":"52.0","longitude":"20.0"},"sensordatavalues":[{"value_type":"temperature","value":"4"}]}]);
        assert_eq!(sensor_points(&j).len(), 1);
    }

    #[test]
    fn planes_skip_ground() {
        let j = json!({"states":[["a","LOT1 ","Poland",1,1,20.0,52.0,9000.0,false,200.0,90.0],["b","X","Poland",1,1,20.0,52.0,null,true,0.0,0.0]]});
        let p = plane_points(&j);
        assert_eq!(p.len(), 1);
        assert_eq!(p[0].name, "LOT1");
    }

    #[test]
    fn warnings_without_list() {
        assert!(warning_items(&json!({"message":"Brak"})).is_empty());
        let w = warning_items(&json!([{"zdarzenie":"Susza","stopie\u{144}":"-1","przebieg":"opis","obszary":[{"opis":"Warta"}]}]));
        assert!(w[0].0.starts_with("Susza"));
        assert!(w[0].1.contains("Warta"));
    }
}
