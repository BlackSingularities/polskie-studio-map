//! Wyszukiwarka (`/`): szczyty, schroniska, przełęcze i miejscowości z okolicy, szlaki (także po kolorze)
//! oraz miejsca w Polsce (Nominatim).

use std::sync::mpsc::{channel, Receiver};
use std::time::{Duration, Instant};

use eframe::egui::{self, RichText};

use crate::fmt;
use crate::geo::BBox;
use crate::osm::{self, Place};
use crate::ui::atlas::{Atlas, Selection};
use crate::ui::theme;

#[derive(Default)]
pub struct Search {
    pub q: String,
    sel: usize,
    typed: Option<Instant>,
    asked: String,
    pending: Option<Receiver<Result<Vec<Place>, String>>>,
    places: Vec<Place>,
    error: Option<String>,
    focused: bool,
}

enum Hit {
    Poi(usize),
    Trail(usize),
    Place(usize),
}

/// Porównanie bez wielkości liter i polskich znaków.
pub fn fold(s: &str) -> String {
    s.chars()
        .flat_map(|c| c.to_lowercase())
        .map(|c| match c {
            'ą' => 'a',
            'ć' => 'c',
            'ę' => 'e',
            'ł' => 'l',
            'ń' => 'n',
            'ó' => 'o',
            'ś' => 's',
            'ź' | 'ż' => 'z',
            'á' | 'ä' => 'a',
            'č' => 'c',
            'ď' => 'd',
            'é' | 'ě' => 'e',
            'í' => 'i',
            'ľ' | 'ĺ' => 'l',
            'ň' => 'n',
            'ô' => 'o',
            'ŕ' | 'ř' => 'r',
            'š' => 's',
            'ť' => 't',
            'ú' | 'ů' | 'ü' => 'u',
            'ý' => 'y',
            'ž' => 'z',
            c => c,
        })
        .collect()
}

const COLOR_WORDS: [(&str, &str); 9] = [
    ("czerwony", "red"),
    ("niebieski", "blue"),
    ("zielony", "green"),
    ("zolty", "yellow"),
    ("czarny", "black"),
    ("pomaranczowy", "orange"),
    ("fioletowy", "purple"),
    ("brazowy", "brown"),
    ("czerwon", "red"),
];

pub fn show(a: &mut Atlas, ctx: &egui::Context) {
    let Some(mut s) = a.search.take() else { return };
    let mut open = true;
    let mut choose: Option<Hit> = None;
    let mut link: Option<String> = None;
    // odpowiedź Nominatim
    if let Some(rx) = &s.pending {
        if let Ok(r) = rx.try_recv() {
            s.pending = None;
            match r {
                Ok(p) => {
                    s.places = p;
                    s.error = None;
                }
                Err(e) => s.error = Some(e),
            }
        } else {
            ctx.request_repaint_after(Duration::from_millis(150));
        }
    }
    let q = fold(s.q.trim());
    if q.chars().count() >= 3 && s.asked != q && s.typed.is_some_and(|t| t.elapsed() > Duration::from_millis(450)) && s.pending.is_none() {
        s.asked = q.clone();
        let (tx, rx) = channel();
        let query = s.q.trim().to_string();
        std::thread::spawn(move || {
            let _ = tx.send(osm::geocode(&query));
        });
        s.pending = Some(rx);
    } else if s.typed.is_some() {
        ctx.request_repaint_after(Duration::from_millis(200));
    }

    // wyniki lokalne
    let mut hits: Vec<(Hit, String, String)> = Vec::new();
    if !q.is_empty() {
        let color = COLOR_WORDS.iter().find(|(pl, _)| q.starts_with(pl) || pl.starts_with(&q) && q.len() >= 4).map(|c| c.1);
        for (i, p) in a.snap.pois.iter().enumerate() {
            if fold(&p.poi.name).contains(&q) {
                let sub = match p.poi.kind {
                    osm::PoiKind::Place => p.poi.kind.label().to_string(),
                    k => format!("{} · {}", k.label(), fmt::ele(p.ele as f64)),
                };
                hits.push((Hit::Poi(i), p.poi.name.clone(), sub));
            }
            if hits.len() > 14 {
                break;
            }
        }
        for (i, r) in a.snap.graph.routes.iter().enumerate() {
            let m = fold(&r.name).contains(&q) || fold(&r.r#ref) == q || color.is_some_and(|c| r.color == c);
            if m {
                hits.push((Hit::Trail(i), r.name.clone(), format!("szlak {}", crate::metrics::TrailColor::parse(&r.color).name())));
            }
            if hits.len() > 24 {
                break;
            }
        }
    }
    for (i, p) in s.places.iter().enumerate() {
        hits.push((Hit::Place(i), p.name.clone(), p.label.clone()));
    }
    if s.sel >= hits.len() {
        s.sel = hits.len().saturating_sub(1);
    }

    egui::Window::new("Szukaj")
        .title_bar(false)
        .collapsible(false)
        .resizable(false)
        .anchor(egui::Align2::LEFT_TOP, [16.0, 16.0])
        .fixed_size([420.0, 10.0])
        .show(ctx, |ui| {
            let r = ui.add(
                egui::TextEdit::singleline(&mut s.q)
                    .hint_text("Szczyt, schronisko, szlak (np. „czerwony”), miejscowość…")
                    .desired_width(f32::INFINITY)
                    .font(egui::TextStyle::Body)
                    .margin(egui::Margin::symmetric(10, 8)),
            );
            if !s.focused {
                r.request_focus();
                s.focused = true;
            }
            if r.changed() {
                s.typed = Some(Instant::now());
                s.places.clear();
                s.sel = 0;
            }
            let (down, up, enter, esc) = ui.input(|i| {
                (
                    i.key_pressed(egui::Key::ArrowDown),
                    i.key_pressed(egui::Key::ArrowUp),
                    i.key_pressed(egui::Key::Enter),
                    i.key_pressed(egui::Key::Escape),
                )
            });
            if down {
                s.sel = (s.sel + 1).min(hits.len().saturating_sub(1));
            }
            if up {
                s.sel = s.sel.saturating_sub(1);
            }
            if esc {
                open = false;
            }
            if enter && s.q.trim().starts_with("psm://") {
                link = Some(s.q.trim().to_string());
            } else if enter && !hits.is_empty() {
                choose = Some(match hits[s.sel].0 {
                    Hit::Poi(i) => Hit::Poi(i),
                    Hit::Trail(i) => Hit::Trail(i),
                    Hit::Place(i) => Hit::Place(i),
                });
            }
            ui.add_space(4.0);
            egui::ScrollArea::vertical().max_height(380.0).show(ui, |ui| {
                for (k, (h, name, sub)) in hits.iter().enumerate() {
                    let resp = theme::card(k == s.sel).inner_margin(egui::Margin::symmetric(10, 6)).show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        ui.label(RichText::new(name).strong());
                        ui.label(RichText::new(sub).small().color(theme::PAPER_3));
                    });
                    if resp.response.interact(egui::Sense::click()).clicked() {
                        choose = Some(match h {
                            Hit::Poi(i) => Hit::Poi(*i),
                            Hit::Trail(i) => Hit::Trail(*i),
                            Hit::Place(i) => Hit::Place(*i),
                        });
                    }
                }
                if s.pending.is_some() {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label(RichText::new("szukam w OpenStreetMap…").small().color(theme::PAPER_3));
                    });
                }
                if let Some(e) = &s.error {
                    ui.label(RichText::new(format!("Wyszukiwanie miejsc niedostępne: {e}")).small().color(theme::WARN));
                }
                if hits.is_empty() && q.len() >= 2 && s.pending.is_none() {
                    theme::para(ui, "Brak wyników w okolicy. Wpisz co najmniej 3 litery, żeby szukać w całej Polsce.");
                }
            });
        });

    if let Some(l) = link {
        if a.apply_link(&l) {
            open = false;
        } else {
            a.notify("Nieprawidłowy link do widoku.");
        }
    }
    match choose {
        Some(Hit::Poi(i)) => {
            let p = a.snap.pois[i].poi.clone();
            a.fly_to(p.lon, p.lat, Some(a.cam.distance.min(7000.0).max(2500.0)));
            a.selection = Selection::Poi(i);
            a.prefs.panel_open = true;
            open = false;
        }
        Some(Hit::Trail(ri)) => {
            let edges: Vec<u32> = a
                .snap
                .graph
                .edges
                .iter()
                .enumerate()
                .filter(|(_, e)| e.routes.contains(&(ri as u32)))
                .map(|(i, _)| i as u32)
                .collect();
            if let Some(b) = edges_bbox(a, &edges) {
                a.fit_bbox(&b);
            }
            if let Some(&e) = edges.first() {
                a.selection = Selection::Trail(e);
                a.prefs.panel_open = true;
            }
            open = false;
        }
        Some(Hit::Place(i)) => {
            let p = s.places[i].clone();
            match p.bbox {
                Some(b) if b.km2() > 2.0 && b.km2() < 200_000.0 => a.fit_bbox(&b),
                _ => a.fly_to(p.lon, p.lat, Some(8000.0)),
            }
            open = false;
        }
        None => {}
    }
    if open {
        a.search = Some(s);
    }
}

pub fn edges_bbox(a: &Atlas, edges: &[u32]) -> Option<BBox> {
    let mut b: Option<BBox> = None;
    for &e in edges {
        let ed = &a.snap.graph.edges[e as usize];
        for i in 0..ed.n() {
            let (lon, lat) = (ed.lon[i], ed.lat[i]);
            b = Some(match b {
                None => BBox::new(lon, lat, lon, lat),
                Some(bb) => BBox::new(bb.w.min(lon), bb.s.min(lat), bb.e.max(lon), bb.n.max(lat)),
            });
        }
    }
    b
}
