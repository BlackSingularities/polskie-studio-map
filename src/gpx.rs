//! Wczytywanie śladów GPX (trkpt / rtept) – pod warstwy użytkownika (etap 7 planu).
#![allow(dead_code)]

pub struct GpxTrack {
    pub name: String,
    /// (dł., szer., wysokość z pliku)
    pub points: Vec<(f64, f64, Option<f32>)>,
}

pub fn parse(text: &str) -> Result<GpxTrack, String> {
    let doc = roxmltree::Document::parse(text).map_err(|_| "Plik nie jest poprawnym GPX.")?;
    let name_of = |parent: &str| {
        doc.descendants()
            .find(|n| n.has_tag_name("name") && n.parent().is_some_and(|p| p.has_tag_name(parent)))
            .and_then(|n| n.text())
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
    };
    let name = name_of("trk")
        .or_else(|| name_of("rte"))
        .or_else(|| name_of("metadata"))
        .unwrap_or_else(|| "Ślad GPX".into());
    let collect = |tag: &str| -> Vec<(f64, f64, Option<f32>)> {
        doc.descendants()
            .filter(|n| n.has_tag_name(tag))
            .filter_map(|n| {
                let lat = n.attribute("lat")?.parse::<f64>().ok()?;
                let lon = n.attribute("lon")?.parse::<f64>().ok()?;
                let e = n
                    .children()
                    .find(|c| c.has_tag_name("ele"))
                    .and_then(|c| c.text())
                    .and_then(|t| t.trim().parse::<f32>().ok());
                (lat.is_finite() && lon.is_finite()).then_some((lon, lat, e))
            })
            .collect()
    };
    let mut points = collect("trkpt");
    if points.is_empty() {
        points = collect("rtept");
    }
    if points.len() < 2 {
        return Err("GPX nie zawiera śladu (trkpt/rtept).".into());
    }
    Ok(GpxTrack { name, points })
}
