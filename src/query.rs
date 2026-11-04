//! „Kliknij i dowiedz się”: w tle pytamy usługi o punkt mapy (wysokość NMT, adres, działka,
//! atrybuty włączonych warstw) oraz szukamy adresów i działek.

use std::sync::{Arc, Mutex};

use crate::geo::BBox;
use crate::layers;
use crate::national::get_bytes;
use crate::osm::{urlencode, Place};

pub type Pairs = Vec<(String, String)>;

/// Wyniki zbierane w miarę nadchodzenia odpowiedzi.
#[derive(Default)]
pub struct PointInfo {
    pub height: Option<f32>,
    pub address: Option<String>,
    pub parcel: Option<Pairs>,
    /// (nazwa warstwy, atrybuty)
    pub layers: Vec<(String, Pairs)>,
    /// Liczba zapytań w toku.
    pub pending: usize,
}

fn get_text(url: &str) -> Option<String> {
    let b = get_bytes(url, 2 << 20).ok()?;
    Some(String::from_utf8_lossy(&b).into_owned())
}

/// Wszystkie zapytania o punkt; wyniki pojawiają się we wspólnej strukturze.
pub fn start(lon: f64, lat: f64, active: &[u16], ctx: eframe::egui::Context) -> Arc<Mutex<PointInfo>> {
    let info = Arc::new(Mutex::new(PointInfo::default()));
    type Job = Box<dyn FnOnce(&Mutex<PointInfo>) + Send>;
    let mut jobs: Vec<Job> = vec![
        Box::new(move |i| {
            let v = height(lon, lat);
            i.lock().unwrap().height = v;
        }),
        Box::new(move |i| {
            let v = address(lon, lat);
            i.lock().unwrap().address = v;
        }),
        Box::new(move |i| {
            let v = parcel(lon, lat);
            i.lock().unwrap().parcel = v;
        }),
    ];
    for &l in active {
        jobs.push(Box::new(move |i| {
            let Some(def) = layers::catalog().layers.get(l as usize) else { return };
            if let Some(p) = def.feature_info(lon, lat).map(|t| parse_info(&t)).filter(|p| !p.is_empty()) {
                i.lock().unwrap().layers.push((def.name.clone(), p));
            }
        }));
    }
    info.lock().unwrap().pending = jobs.len();
    for job in jobs {
        let (i, c) = (info.clone(), ctx.clone());
        std::thread::spawn(move || {
            job(&i);
            i.lock().unwrap().pending -= 1;
            c.request_repaint();
        });
    }
    info
}

/// Długość, szerokość → PUWG 1992: (x na północ, y na wschód).
pub fn puwg92(lon: f64, lat: f64) -> (f64, f64) {
    const A: f64 = 6_378_137.0;
    const F: f64 = 1.0 / 298.257_222_101;
    let e2 = F * (2.0 - F);
    let ep2 = e2 / (1.0 - e2);
    let (phi, dl) = (lat.to_radians(), (lon - 19.0).to_radians());
    let (s, c, t2) = (phi.sin(), phi.cos(), phi.tan().powi(2));
    let n = A / (1.0 - e2 * s * s).sqrt();
    let eta2 = ep2 * c * c;
    let l = dl * c;
    let m = A
        * ((1.0 - e2 / 4.0 - 3.0 * e2 * e2 / 64.0 - 5.0 * e2.powi(3) / 256.0) * phi
            - (3.0 * e2 / 8.0 + 3.0 * e2 * e2 / 32.0 + 45.0 * e2.powi(3) / 1024.0) * (2.0 * phi).sin()
            + (15.0 * e2 * e2 / 256.0 + 45.0 * e2.powi(3) / 1024.0) * (4.0 * phi).sin()
            - (35.0 * e2.powi(3) / 3072.0) * (6.0 * phi).sin());
    let k0 = 0.9993;
    let east = k0 * n * (l + (1.0 - t2 + eta2) * l.powi(3) / 6.0 + (5.0 - 18.0 * t2 + t2 * t2) * l.powi(5) / 120.0);
    let north = k0 * (m + n * phi.tan() * (l * l / 2.0 + (5.0 - t2 + 9.0 * eta2) * l.powi(4) / 24.0));
    (north - 5_300_000.0, east + 500_000.0)
}

fn height(lon: f64, lat: f64) -> Option<f32> {
    let (x, y) = puwg92(lon, lat);
    let t = get_text(&format!("https://services.gugik.gov.pl/nmt/?request=GetHbyXY&x={x:.2}&y={y:.2}"))?;
    t.trim().parse::<f32>().ok().filter(|h| *h > -100.0 && *h < 3000.0)
}

fn address_label(r: &serde_json::Value) -> Option<String> {
    let s = |k: &str| r[k].as_str().filter(|v| !v.is_empty());
    let city = s("city")?;
    let num = s("number").unwrap_or("");
    Some(match s("street") {
        Some(st) => format!("{st} {num}, {city}").replace(" ,", ","),
        None => format!("{city} {num}").trim().to_string(),
    })
}

fn address(lon: f64, lat: f64) -> Option<String> {
    let t = get_text(&format!("https://services.gugik.gov.pl/uug/?request=GetAddressReverse&location=POINT({lon:.6}%20{lat:.6})&srid=4326"))?;
    let v: serde_json::Value = serde_json::from_str(&t).ok()?;
    address_label(&v["results"]["1"])
}

/// Odpowiedź ULDK: pierwszy wiersz to stan (liczba wyników albo -1), dalej pola rozdzielone „|”.
fn uldk_rows(t: &str) -> Vec<Vec<String>> {
    let mut l = t.lines();
    if l.next().and_then(|s| s.trim().parse::<i32>().ok()).is_none_or(|n| n < 0) {
        return Vec::new();
    }
    l.filter(|r| r.contains('|')).map(|r| r.split('|').map(|f| f.trim().to_string()).collect()).collect()
}

fn parcel(lon: f64, lat: f64) -> Option<Pairs> {
    let t = get_text(&format!("https://uldk.gugik.gov.pl/?request=GetParcelByXY&xy={lon:.6},{lat:.6},4326&result=teryt,voivodeship,county,commune,region,parcel"))?;
    let row = uldk_rows(&t).into_iter().next()?;
    let names = ["Identyfikator", "Województwo", "Powiat", "Gmina", "Obręb", "Numer działki"];
    Some(names.iter().zip(row).map(|(k, v)| (k.to_string(), v)).collect())
}

/// Prostokąt obejmujący geometrię WKT (w Polsce długość jest zawsze mniejsza od szerokości).
fn wkt_bbox(wkt: &str) -> Option<BBox> {
    let body = wkt.split('(').skip(1).collect::<Vec<_>>().join(" ").replace([')', ','], " ");
    let nums: Vec<f64> = body.split_whitespace().filter_map(|n| n.parse().ok()).collect();
    let mut b: Option<BBox> = None;
    for p in nums.chunks_exact(2) {
        let (lon, lat) = (p[0].min(p[1]), p[0].max(p[1]));
        b = Some(match b {
            None => BBox::new(lon, lat, lon, lat),
            Some(o) => BBox::new(o.w.min(lon), o.s.min(lat), o.e.max(lon), o.n.max(lat)),
        });
    }
    b
}

fn search_parcels(q: &str) -> Vec<Place> {
    let req = if q.contains('_') { "GetParcelById" } else { "GetParcelByIdOrNr" };
    let url = format!(
        "https://uldk.gugik.gov.pl/?request={req}&id={}&result=teryt,commune,region,parcel,geom_wkt&srid=4326",
        urlencode(q)
    );
    let Some(t) = get_text(&url) else { return Vec::new() };
    uldk_rows(&t)
        .into_iter()
        .take(8)
        .filter_map(|r| {
            let [id, commune, region, nr, wkt] = r.as_slice() else { return None };
            let b = wkt_bbox(wkt)?;
            let c = b.center();
            Some(Place { name: format!("Działka {nr}"), label: format!("{commune}, obręb {region} · {id}"), lon: c[0], lat: c[1], bbox: Some(b) })
        })
        .collect()
}

fn search_addresses(q: &str) -> Vec<Place> {
    let url = format!("https://services.gugik.gov.pl/uug/?request=GetAddress&address={}&srid=4326", urlencode(q));
    let Some(v) = get_text(&url).and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok()) else { return Vec::new() };
    let Some(res) = v["results"].as_object() else { return Vec::new() };
    res.values()
        .filter_map(|r| {
            let parse = |k: &str| r[k].as_str()?.parse::<f64>().ok();
            Some(Place {
                name: address_label(r)?,
                label: format!("adres · {}", r["code"].as_str().unwrap_or("")),
                lon: parse("x")?,
                lat: parse("y")?,
                bbox: None,
            })
        })
        .take(6)
        .collect()
}

/// Adresy i działki pasujące do zapytania z cyfrą (nazwy miejsc obsługuje Nominatim).
pub fn search(q: &str) -> Vec<Place> {
    if !q.chars().any(|c| c.is_ascii_digit()) {
        return Vec::new();
    }
    let mut out = search_addresses(q);
    out.extend(search_parcels(q));
    out
}

fn useful(k: &str, v: &str) -> bool {
    let (kl, vl) = (k.to_lowercase(), v.to_lowercase());
    !v.is_empty()
        && !matches!(vl.as_str(), "null" | "polygon" | "point" | "multipolygon" | "polyline")
        && !kl.contains("shape")
        && !kl.contains("geom")
        && !kl.contains("gdb_id")
        && !kl.contains("objectid")
}

/// Odpowiedź GetFeatureInfo: usługi używają różnych odmian „tekstu”.
pub fn parse_info(t: &str) -> Pairs {
    let t = t.trim();
    let mut out: Pairs = Vec::new();
    if t.starts_with("<?xml") || t.starts_with("<FeatureCollection") {
        // GeoMedia: <Attribute Name="…">wartość</Attribute>
        if let Ok(doc) = roxmltree::Document::parse(t) {
            for n in doc.descendants().filter(|n| n.has_tag_name("Attribute")) {
                if let (Some(k), Some(v)) = (n.attribute("Name"), n.text()) {
                    out.push((k.to_string(), v.trim().to_string()));
                }
            }
        }
    } else if t.contains('@') && t.contains(';') {
        // „@Tytuł nagłówek;nagłówek; wartość;wartość;”
        for part in t.split('@').skip(1) {
            let part = part.trim_end_matches("<hr/>").trim();
            let Some((head, vals)) = part.split_once(|c: char| c == '\n' || c == '\r').filter(|(h, _)| h.contains(';')).or_else(|| part.split_once("; ")) else { continue };
            let start = head[..head.find(';').unwrap_or(head.len())].rfind(' ').map_or(0, |i| i + 1);
            let keys: Vec<&str> = head[start..].split(';').map(str::trim).filter(|k| !k.is_empty()).collect();
            let values: Vec<&str> = vals.trim().split(';').map(str::trim).collect();
            for (k, v) in keys.iter().zip(values) {
                out.push((k.to_string(), v.to_string()));
            }
        }
    } else if !t.starts_with('<') {
        // MapServer, GeoServer: „nazwa = 'wartość'”
        for l in t.lines() {
            if let Some((k, v)) = l.split_once(" = ") {
                out.push((k.trim().to_string(), v.trim().trim_matches('\'').to_string()));
            }
        }
    }
    out.retain(|(k, v)| useful(k, v));
    out.truncate(30);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn puwg92_krakow() {
        // Rynek Główny w Krakowie
        let (x, y) = puwg92(19.9383, 50.0614);
        assert!((x - 244_000.0).abs() < 1500.0, "{x}");
        assert!((y - 567_000.0).abs() < 1500.0, "{y}");
    }

    #[test]
    fn parses_info_variants() {
        let a = parse_info("@Nadleśnictwa inspectorate_id;Shape;inspectorate_name; 20013;Polygon;Krzeszowice   ;");
        assert_eq!(a, vec![("inspectorate_id".to_string(), "20013".to_string()), ("inspectorate_name".to_string(), "Krzeszowice".to_string())]);
        let b = parse_info("<?xml version=\"1.0\"?><FeatureCollection><Layer><Attribute Name=\"Gmina\">Kraków</Attribute></Layer></FeatureCollection>");
        assert_eq!(b[0].1, "Kraków");
        let c = parse_info("Layer 'x'\n  Feature 1:\n    nazwa = 'Tatry'\n");
        assert_eq!(c[0], ("nazwa".to_string(), "Tatry".to_string()));
        assert!(parse_info("no features were found").is_empty());
        assert!(parse_info("<html>błąd</html>").is_empty());
    }

    #[test]
    fn uldk_and_wkt() {
        let r = uldk_rows("0\n126105_9.0001.580/4|Kraków|S-1|580/4|SRID=4326;POLYGON((19.9 50.0,19.91 50.01))\n");
        assert_eq!(r[0][3], "580/4");
        assert!(uldk_rows("-1 brak wyników\n").is_empty());
        let b = wkt_bbox("SRID=4326;POLYGON((50.0 19.9,50.01 19.91))").unwrap();
        assert!(b.w < 20.0 && b.n > 50.0);
    }
}
