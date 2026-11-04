//! Interfejs nad mapą: wyszukiwarka, pasek narzędzi, szuflada (szczegóły, warstwy, światło), sterowanie kamerą, profil i pomoc.

use eframe::egui::{self, Color32, RichText};

use crate::fmt;
use crate::geo::{bearing, haversine};
use crate::metrics::{self, SAC_LABEL};
use crate::osm::PoiKind;
use crate::sun::{self, Day};
use crate::ui::atlas::{Atlas, Lens, Quality, Selection, Style, Tool};
use crate::ui::theme;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tab {
    Info,
}

fn metric(ui: &mut egui::Ui, k: &str, v: String) {
    ui.vertical(|ui| {
        ui.add(egui::Label::new(RichText::new(k.to_string()).size(11.0).color(theme::PAPER_3)).extend());
        ui.add(egui::Label::new(RichText::new(v).size(15.0).strong()).extend());
    });
}

fn grid2(ui: &mut egui::Ui, items: Vec<(&str, String)>) {
    egui::Grid::new(ui.next_auto_id()).num_columns(2).spacing([18.0, 8.0]).show(ui, |ui| {
        for (i, (k, v)) in items.into_iter().enumerate() {
            metric(ui, k, v);
            if i % 2 == 1 {
                ui.end_row();
            }
        }
    });
}

fn big(ui: &mut egui::Ui, v: String, sub: &str, color: Color32) {
    ui.horizontal(|ui| {
        ui.label(RichText::new(v).size(30.0).strong().color(color));
        ui.label(RichText::new(sub).color(theme::PAPER_3));
    });
}

/// Wyszukiwarka w lewym górnym rogu; po kliknięciu rozwija się w tym samym miejscu.
pub fn search_pill(a: &mut Atlas, ctx: &egui::Context) {
    if a.search.is_some() {
        return;
    }
    egui::Area::new(egui::Id::new("szukaj-pole"))
        .fixed_pos([16.0, 16.0])
        .show(ctx, |ui| {
            theme::glass().show(ui, |ui| {
                ui.set_width(RAIL_X + DRAWER_W - 28.0);
                if theme::row_button(ui, theme::icon::MAGNIFYING_GLASS, "Szukaj miejsca, szczytu, szlaku", false).on_hover_text("/").clicked() {
                    a.search = Some(Default::default());
                }
            });
        });
}

const RAIL_X: f32 = 16.0;
const TOP_Y: f32 = 76.0;
const DRAWER_W: f32 = 372.0;

/// Pionowy pasek narzędzi po lewej.
pub fn rail(a: &mut Atlas, ctx: &egui::Context) {
    egui::Area::new(egui::Id::new("narzedzia"))
        .fixed_pos([RAIL_X, TOP_Y])
        .show(ctx, |ui| {
            theme::glass().show(ui, |ui| {
                ui.vertical(|ui| {
                    let open_tab = |a: &mut Atlas, tab: Tab| {
                        a.prefs.panel_open = !(a.prefs.panel_open && a.tab == tab);
                        a.tab = tab;
                    };
                    if theme::icon_button(ui, theme::icon::COMPASS, a.tool == Tool::Explore && a.prefs.panel_open && a.tab == Tab::Info).on_hover_text("Szczegóły okolicy (1)").clicked() {
                        a.tool = Tool::Explore;
                        open_tab(a, Tab::Info);
                    }
                    ui.separator();
                    if theme::icon_button(ui, theme::icon::STACK, a.layers_open).on_hover_text("Warstwy").clicked() {
                        a.layers_open = !a.layers_open;
                    }
                    if theme::icon_button(ui, theme::icon::RULER, a.tool == Tool::Measure).on_hover_text("Pomiar i linia wzroku (2)").clicked() {
                        a.tool = if a.tool == Tool::Measure { Tool::Explore } else { Tool::Measure };
                        a.prefs.panel_open = true;
                        a.tab = Tab::Info;
                    }
                    ui.separator();
                    if theme::icon_button(ui, theme::icon::WAVES, a.lens == Lens::Band).on_hover_text("Ta sama wysokość (H)").clicked() {
                        a.set_lens(Lens::Band);
                    }
                    if theme::icon_button(ui, theme::icon::PLUS_MINUS, a.lens == Lens::Rel).on_hover_text("Wysokość względna (W)").clicked() {
                        a.set_lens(Lens::Rel);
                    }
                    if theme::icon_button(ui, theme::icon::EYE, a.lens == Lens::Vis).on_hover_text("Co widać stąd (O)").clicked() {
                        a.set_lens(Lens::Vis);
                    }
                    ui.separator();
                    if theme::icon_button(ui, theme::icon::MOUNTAINS, a.panorama.is_some()).on_hover_text("Panorama ze środka widoku").clicked() {
                        if a.panorama.is_some() {
                            a.exit_panorama();
                        } else {
                            let (lon, lat) = (a.cam.lon, a.cam.lat);
                            a.enter_panorama(lon, lat);
                        }
                    }
                });
            });
        });
}

/// Panel warstw: zestawy, włączone warstwy (krycie, kolejność, legenda) i katalog z wyszukiwarką.
pub fn layers_panel(a: &mut Atlas, ctx: &egui::Context) {
    use crate::layers::{self, catalog};
    // legendy pobrane w tle
    let inbox: Vec<_> = std::mem::take(&mut *a.legend_inbox.lock().unwrap());
    for (i, img) in inbox {
        let tex = img.map(|m| {
            let size = [m.width() as usize, m.height() as usize];
            ctx.load_texture(format!("legenda-{i}"), egui::ColorImage::from_rgba_unmultiplied(size, m.as_raw()), egui::TextureOptions::LINEAR)
        });
        a.legends.insert(i, tex);
    }
    if !a.layers_open {
        return;
    }
    let mut open = true;
    egui::Window::new("Warstwy")
        .open(&mut open)
        .collapsible(false)
        .resizable(false)
        .fixed_pos([82.0, TOP_Y])
        .fixed_size([DRAWER_W, 620.0])
        .show(ctx, |ui| {
            ui.add(egui::TextEdit::singleline(&mut a.layer_query).hint_text(format!("Szukaj wśród {} warstw…", catalog().layers.len())).desired_width(f32::INFINITY));
            ui.add_space(4.0);
            egui::ScrollArea::vertical().auto_shrink([false, false]).max_height(590.0).show(ui, |ui| {
                ui.label(theme::kicker("Zestawy"));
                let builtin = catalog().presets.clone();
                let mut pick = None;
                ui.horizontal_wrapped(|ui| {
                    for p in builtin.iter().chain(a.user_presets.iter()) {
                        if ui.small_button(&p.name).clicked() {
                            pick = Some(p.layers.clone());
                        }
                    }
                    if ui.small_button("Wyłącz wszystkie").clicked() {
                        pick = Some(Vec::new());
                    }
                });
                if let Some(ids) = pick {
                    for l in &mut a.prefs.layers {
                        l.on = ids.contains(&l.id);
                    }
                }
                ui.horizontal(|ui| {
                    ui.add(egui::TextEdit::singleline(&mut a.new_preset).hint_text("Nazwa własnego zestawu").desired_width(200.0));
                    if ui.add_enabled(!a.new_preset.trim().is_empty(), egui::Button::new("Zapisz")).clicked() {
                        let ids = a.prefs.layers.iter().filter(|l| l.on).map(|l| l.id.clone()).collect();
                        let name = a.new_preset.trim().to_string();
                        a.user_presets.retain(|p| p.name != name);
                        a.user_presets.push(layers::Preset { name, layers: ids });
                        a.new_preset.clear();
                        if let Err(e) = layers::save_presets(&a.user_presets) {
                            a.notify(format!("Nie udało się zapisać zestawu: {e}"));
                        }
                    }
                });
                ui.add_space(6.0);
                feeds_ui(a, ui);
                active_layers_ui(a, ui);
                let q = crate::ui::search::fold(a.layer_query.trim());
                let mut groups: Vec<&str> = Vec::new();
                for d in &catalog().layers {
                    if !groups.contains(&d.group.as_str()) {
                        groups.push(&d.group);
                    }
                }
                for g in groups {
                    let list: Vec<&layers::LayerDef> = catalog()
                        .layers
                        .iter()
                        .filter(|d| d.group == g && (q.is_empty() || crate::ui::search::fold(&d.name).contains(&q) || crate::ui::search::fold(g).contains(&q)))
                        .collect();
                    if list.is_empty() {
                        continue;
                    }
                    egui::CollapsingHeader::new(format!("{g} ({})", list.len()))
                        .open(if q.is_empty() { None } else { Some(true) })
                        .show(ui, |ui| {
                            for d in list {
                                let Some(st) = a.prefs.layers.iter_mut().find(|l| l.id == d.id) else { continue };
                                let tip = format!("{}Źródło: {}\nWidoczna od poziomu {}", if d.desc.is_empty() { String::new() } else { format!("{}\n", d.desc) }, d.attribution, d.min_z);
                                ui.checkbox(&mut st.on, d.name.as_str()).on_hover_text(tip);
                            }
                        });
                }
            });
        });
    if !open {
        a.layers_open = false;
    }
}

/// Dane na żywo: przełączniki, stan pobierania i ostrzeżenia.
fn feeds_ui(a: &mut Atlas, ui: &mut egui::Ui) {
    ui.label(theme::kicker("Na żywo"));
    for def in &crate::feeds::FEEDS {
        let mut on = a.prefs.feeds.iter().any(|f| f == def.id);
        if ui.checkbox(&mut on, def.name).on_hover_text(format!("Źródło: {}", def.attribution)).changed() {
            a.prefs.feeds.retain(|f| f != def.id);
            if on {
                a.prefs.feeds.push(def.id.to_string());
            }
        }
        if !on {
            continue;
        }
        let st = a.feeds.status(def.id);
        let (t, c) = if let Some(e) = st.error {
            (format!("serwis niedostępny: {e}"), theme::WARN)
        } else if st.busy {
            ("pobieranie…".to_string(), theme::PAPER_3)
        } else {
            (format!("{} · {} min temu", st.count, st.age_min.unwrap_or(0)), theme::PAPER_3)
        };
        ui.label(RichText::new(t).small().color(c));
        for (title, body) in a.feeds.warnings(def.id).iter() {
            egui::CollapsingHeader::new(RichText::new(title).small()).show(ui, |ui| {
                ui.label(RichText::new(body).small());
            });
        }
    }
    ui.separator();
}

/// Punkt danych na żywo: wartości i przebieg od uruchomienia programu.
fn feed_info(a: &mut Atlas, ui: &mut egui::Ui, id: &str, key: &str) {
    let points = a.feeds.points(id);
    let Some(p) = points.iter().find(|p| p.key == key) else {
        a.selection = Selection::None;
        return;
    };
    let source = crate::feeds::FEEDS.iter().find(|f| f.id == id).map_or("", |f| f.name);
    close_row(a, ui, source);
    ui.heading(&p.name);
    if !p.label.is_empty() {
        ui.label(RichText::new(&p.label).size(20.0).strong());
    }
    for (k, v) in &p.details {
        ui.horizontal_wrapped(|ui| {
            ui.label(RichText::new(k).small().color(theme::PAPER_3));
            ui.label(v);
        });
    }
    let h = a.feeds.history(id, key);
    ui.add_space(8.0);
    ui.label(theme::kicker("Od uruchomienia programu"));
    if h.len() < 2 {
        theme::para(ui, "Wykres pojawi się po kolejnym odświeżeniu danych.");
    } else {
        let (rect, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 70.0), egui::Sense::hover());
        let (lo, hi) = h.iter().fold((f32::MAX, f32::MIN), |m, v| (m.0.min(*v), m.1.max(*v)));
        let span = (hi - lo).max(0.001);
        let pts: Vec<egui::Pos2> = h
            .iter()
            .enumerate()
            .map(|(i, v)| egui::pos2(rect.left() + rect.width() * i as f32 / (h.len() - 1) as f32, rect.bottom() - rect.height() * (v - lo) / span))
            .collect();
        ui.painter().add(egui::Shape::line(pts, egui::Stroke::new(2.0, theme::TEAL)));
        ui.label(RichText::new(format!("od {lo:.1} do {hi:.1}")).small().color(theme::PAPER_3));
    }
    let (lon, lat) = (p.lon, p.lat);
    ui.add_space(6.0);
    point_buttons(a, ui, lon, lat);
}

/// Włączone warstwy od góry stosu.
fn active_layers_ui(a: &mut Atlas, ui: &mut egui::Ui) {
    use crate::layers;
    let on: Vec<usize> = (0..a.prefs.layers.len()).rev().filter(|&i| a.prefs.layers[i].on).collect();
    if on.is_empty() {
        return;
    }
    ui.label(theme::kicker(&format!("Włączone ({})", on.len())));
    let (loading, drawn) = match &a.renderer {
        Some(r) => (r.tiles.layer_loading.clone(), r.tiles.layer_drawn.clone()),
        None => Default::default(),
    };
    let mut swap = None;
    for (k, &i) in on.iter().enumerate() {
        let Some(ci) = layers::index_of(&a.prefs.layers[i].id) else { continue };
        let d = &layers::catalog().layers[ci as usize];
        ui.horizontal(|ui| {
            if ui.small_button(theme::icon::X).on_hover_text("Wyłącz").clicked() {
                a.prefs.layers[i].on = false;
            }
            ui.label(RichText::new(d.name.as_str()).strong());
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if k + 1 < on.len() && ui.small_button("↓").on_hover_text("Niżej").clicked() {
                    swap = Some((i, on[k + 1]));
                }
                if k > 0 && ui.small_button("↑").on_hover_text("Wyżej").clicked() {
                    swap = Some((i, on[k - 1]));
                }
            });
        });
        ui.add(egui::Slider::new(&mut a.prefs.layers[i].opacity, 0.05..=1.0).text("krycie").custom_formatter(|v, _| format!("{:.0}%", v * 100.0)));
        let status = if let Some(e) = layers::error_of(ci) {
            Some((format!("serwis niedostępny: {e}"), theme::WARN))
        } else if loading.get(&ci).copied().unwrap_or(0) > 0 {
            Some((format!("ładowanie · {}", loading[&ci]), theme::PAPER_3))
        } else if drawn.get(&ci).copied().unwrap_or(0) == 0 {
            Some((format!("nic w tym widoku; przybliż (od poziomu {})", d.min_z), theme::PAPER_3))
        } else {
            None
        };
        if let Some((t, c)) = status {
            ui.label(RichText::new(t).small().color(c));
        }
        let header = egui::CollapsingHeader::new(RichText::new("Legenda").small()).id_salt(("legenda", ci)).show(ui, |ui| match a.legends.get(&ci) {
            Some(Some(tex)) => {
                let s = tex.size_vec2().min(egui::vec2(320.0, 360.0));
                ui.image((tex.id(), s));
            }
            Some(None) => {
                ui.label(RichText::new("Usługa nie udostępnia legendy.").small().color(theme::PAPER_3));
            }
            None => {
                ui.spinner();
            }
        });
        if header.body_returned.is_some() && !a.legends.contains_key(&ci) && !a.legend_asked.contains(&ci) {
            a.legend_asked.insert(ci);
            let (inbox, c) = (a.legend_inbox.clone(), ui.ctx().clone());
            std::thread::spawn(move || {
                let img = layers::fetch_legend(ci);
                inbox.lock().unwrap().push((ci, img));
                c.request_repaint();
            });
        }
        ui.add_space(4.0);
    }
    if let Some((x, y)) = swap {
        a.prefs.layers.swap(x, y);
    }
    ui.separator();
}

/// Szerokość prawego panelu szczegółów.
const SIDEBAR_W: f32 = 440.0;

fn sidebar_open(a: &Atlas) -> bool {
    a.prefs.panel_open && a.tab == Tab::Info
}

/// Odsunięcie elementów przy prawej krawędzi, gdy panel szczegółów jest otwarty.
pub fn right_inset(a: &Atlas) -> f32 {
    if sidebar_open(a) { SIDEBAR_W } else { 0.0 }
}

/// Szczegóły w prawym panelu bocznym (zwęża widok mapy); wywoływać przed panelem środkowym.
pub fn sidebar(a: &mut Atlas, ctx: &egui::Context) {
    if !sidebar_open(a) {
        // schowany panel: uchwyt przy prawej krawędzi
        egui::Area::new(egui::Id::new("szczegoly-uchwyt"))
            .anchor(egui::Align2::RIGHT_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                theme::glass().inner_margin(egui::Margin::symmetric(4, 10)).show(ui, |ui| {
                    if ui.small_button(theme::icon::CARET_LEFT).on_hover_text("Pokaż szczegóły (P)").clicked() {
                        a.prefs.panel_open = true;
                        a.tab = Tab::Info;
                    }
                });
            });
        return;
    }
    egui::SidePanel::right("szczegoly")
        .exact_width(SIDEBAR_W)
        .resizable(false)
        .frame(egui::Frame::new().fill(Color32::from_rgb(22, 25, 30)).inner_margin(egui::Margin::symmetric(20, 18)))
        .show(ctx, |ui| {
            // większe pismo niż w szufladzie
            let st = ui.style_mut();
            st.text_styles.insert(egui::TextStyle::Body, egui::FontId::proportional(15.0));
            st.text_styles.insert(egui::TextStyle::Button, egui::FontId::proportional(15.0));
            st.text_styles.insert(egui::TextStyle::Heading, egui::FontId::proportional(27.0));
            st.text_styles.insert(egui::TextStyle::Monospace, egui::FontId::monospace(14.0));
            st.text_styles.insert(egui::TextStyle::Small, egui::FontId::proportional(12.5));
            st.spacing.item_spacing.y = 7.0;
            ui.horizontal(|ui| {
                ui.label(RichText::new("Szczegóły").size(17.0).strong());
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.small_button(theme::icon::CARET_RIGHT).on_hover_text("Ukryj panel (P)").clicked() {
                        a.prefs.panel_open = false;
                    }
                });
            });
            ui.separator();
            egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| info(a, ui));
        });
}


/// Sterowanie kamerą w prawym dolnym rogu.
pub fn nav(a: &mut Atlas, ctx: &egui::Context) {
    egui::Area::new(egui::Id::new("kamera"))
        .anchor(egui::Align2::RIGHT_BOTTOM, [-16.0 - right_inset(a), -40.0])
        .show(ctx, |ui| {
            theme::glass().show(ui, |ui| {
                ui.vertical(|ui| {
                    let zoom = |a: &mut Atlas, k: f64| {
                        let mut to = a.cam;
                        to.distance = (a.cam.distance * k).clamp(a.min_dist(), 900_000.0);
                        a.start_fly(to, 0.35);
                    };
                    if theme::icon_button(ui, theme::icon::PLUS, false).on_hover_text("Przybliż").clicked() {
                        zoom(a, 0.55);
                    }
                    if theme::icon_button(ui, theme::icon::MINUS, false).on_hover_text("Oddal").clicked() {
                        zoom(a, 1.8);
                    }
                    ui.separator();
                    if theme::icon_button(ui, theme::icon::PERSPECTIVE, a.cam.pitch < 75.0).on_hover_text("Widok ukośny / z góry (V)").clicked() {
                        let mut to = a.cam;
                        to.pitch = if a.cam.pitch > 75.0 { 40.0 } else { 89.0 };
                        a.start_fly(to, 0.7);
                    }
                });
            });
        });
}

fn info(a: &mut Atlas, ui: &mut egui::Ui) {
    if a.lens_pin.is_some() {
        return lens_info(a, ui);
    }
    match a.tool {
        Tool::Measure => return measure_info(a, ui),
        Tool::Explore => {}
    }
    match a.selection.clone() {
        Selection::Poi(i) => poi_info(a, ui, i),
        Selection::Point { lon, lat } => point_info(a, ui, lon, lat),
        Selection::Trail(e) => trail_info(a, ui, e),
        Selection::Feed(id, key) => feed_info(a, ui, &id, &key),
        Selection::None => region_info(a, ui),
    }
}

fn region_info(a: &mut Atlas, ui: &mut egui::Ui) {
    ui.label(theme::kicker(&format!("Okolica · {}", fmt::decimal(a.cam.lon, a.cam.lat))));
    let name = a
        .snap
        .pois
        .iter()
        .filter(|p| p.poi.kind == PoiKind::Peak)
        .min_by(|x, y| {
            haversine(a.cam.lon, a.cam.lat, x.poi.lon, x.poi.lat).total_cmp(&haversine(a.cam.lon, a.cam.lat, y.poi.lon, y.poi.lat))
        })
        .map(|p| format!("Okolice: {}", p.poi.name))
        .unwrap_or_else(|| "Polska".into());
    ui.heading(name);
    let g = &a.snap.graph;
    if g.edges.is_empty() {
        let (busy, err) = a.world.status();
        theme::para(ui, if busy.is_some() {
            "Pobieram szlaki i punkty tej okolicy z OpenStreetMap…"
        } else if a.cam.distance > 90_000.0 {
            "Przybliż widok, żeby wczytać szlaki i punkty okolicy."
        } else if let Some(e) = &err {
            e.as_str()
        } else {
            "W tej okolicy nie ma znakowanych szlaków w OpenStreetMap."
        });
    } else {
        grid2(ui, vec![
            ("Szlaki", fmt::dist(g.total_km * 1000.0)),
            ("Trasy PTTK i inne", fmt::int(g.routes.len() as f64)),
            ("Punkty", fmt::int(a.snap.pois.len() as f64)),
            ("Sektory", a.snap.sectors.len().to_string()),
        ]);
    }
    ui.add_space(10.0);
    ui.label(theme::kicker("Najbardziej wybitne szczyty"));
    let peaks: Vec<(usize, String, f32)> = a
        .snap
        .pois
        .iter()
        .enumerate()
        .filter(|(_, p)| p.poi.kind == PoiKind::Peak)
        .take(14)
        .map(|(i, p)| (i, p.poi.name.clone(), p.ele))
        .collect();
    for (i, n, e) in peaks {
        if row(ui, &n, &fmt::ele(e as f64), "") {
            let p = &a.snap.pois[i].poi;
            let (lon, lat) = (p.lon, p.lat);
            a.selection = Selection::Poi(i);
            a.fly_to(lon, lat, Some(a.cam.distance.clamp(2500.0, 9000.0)));
        }
    }
    ui.add_space(10.0);
    if theme::row_button(ui, theme::icon::LINK, "Kopiuj link do widoku", false).clicked() {
        let l = a.share_link();
        ui.ctx().copy_text(l);
        a.notify("Skopiowano link do widoku – wklej go do wyszukiwarki programu albo otwórz w systemie.");
    }
}

fn row(ui: &mut egui::Ui, name: &str, right: &str, sub: &str) -> bool {
    let r = ui
        .horizontal(|ui| {
            ui.set_width(ui.available_width());
            let l = ui.add(egui::Label::new(RichText::new(name).strong()).sense(egui::Sense::click()));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.label(RichText::new(right).monospace().color(theme::PAPER_2));
                if !sub.is_empty() {
                    ui.label(RichText::new(sub).small().color(theme::PAPER_3));
                }
            });
            l.clicked()
        })
        .inner;
    r
}

fn point_buttons(a: &mut Atlas, ui: &mut egui::Ui, lon: f64, lat: f64) {
    if theme::row_button(ui, theme::icon::EYE, "Co widać stąd?", false).clicked() {
        a.pin_lens(lon, lat, Lens::Vis);
    }
    if theme::row_button(ui, theme::icon::PLUS_MINUS, "Wysokość względna", false).clicked() {
        a.pin_lens(lon, lat, Lens::Rel);
    }
    if theme::row_button(ui, theme::icon::WAVES, "Ta sama wysokość", false).clicked() {
        a.pin_lens(lon, lat, Lens::Band);
    }
    if theme::row_button(ui, theme::icon::MOUNTAINS, "Panorama", false).clicked() {
        a.enter_panorama(lon, lat);
    }
    if theme::row_button(ui, theme::icon::COPY, "Kopiuj współrzędne", false).clicked() {
        ui.ctx().copy_text(fmt::decimal(lon, lat));
        a.notify("Skopiowano współrzędne.");
    }
}

fn close_row(a: &mut Atlas, ui: &mut egui::Ui, kicker: &str) {
    ui.horizontal(|ui| {
        ui.label(theme::kicker(kicker));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui.small_button(theme::icon::X).clicked() {
                a.selection = Selection::None;
            }
        });
    });
}

fn poi_info(a: &mut Atlas, ui: &mut egui::Ui, i: usize) {
    let Some(p) = a.snap.pois.get(i).cloned() else {
        a.selection = Selection::None;
        return;
    };
    close_row(a, ui, p.poi.kind.label());
    ui.heading(&p.poi.name);
    let mut items = vec![("Wysokość", fmt::ele(p.ele as f64)), ("Model terenu", fmt::ele(p.dem as f64))];
    if p.poi.kind == PoiKind::Place && p.poi.pop > 0 {
        items.push(("Mieszkańcy", fmt::int(p.poi.pop as f64)));
    }
    items.push(("Odległość", fmt::dist(haversine(a.cam.lon, a.cam.lat, p.poi.lon, p.poi.lat))));
    grid2(ui, items);
    ui.label(RichText::new(fmt::coords(p.poi.lon, p.poi.lat)).monospace().small().color(theme::PAPER_3));
    if p.poi.ele.is_some() && (p.ele - p.dem).abs() > 25.0 {
        theme::para(ui, "Model terenu wygładza najostrzejsze granie – wysokość szczytu pochodzi z OSM.");
    }
    ui.horizontal(|ui| {
        if !p.poi.osm.is_empty() && ui.link("OpenStreetMap").clicked() {
            let _ = open::that(format!("https://www.openstreetmap.org/{}", p.poi.osm));
        }
        if !p.poi.wiki.is_empty() {
            if let Some((lang, title)) = p.poi.wiki.split_once(':') {
                if ui.link("Wikipedia").clicked() {
                    let _ = open::that(format!("https://{lang}.wikipedia.org/wiki/{}", crate::osm::urlencode(title)));
                }
            }
        }
    });
    ui.add_space(6.0);
    point_buttons(a, ui, p.poi.lon, p.poi.lat);
    if p.poi.kind == PoiKind::Peak {
        ui.add_space(10.0);
        ui.label(theme::kicker("Sąsiednie szczyty"));
        let mut near: Vec<(usize, f64)> = a
            .snap
            .pois
            .iter()
            .enumerate()
            .filter(|(j, q)| *j != i && q.poi.kind == PoiKind::Peak)
            .map(|(j, q)| (j, haversine(p.poi.lon, p.poi.lat, q.poi.lon, q.poi.lat)))
            .filter(|(_, d)| *d < 6000.0)
            .collect();
        near.sort_by(|x, y| x.1.total_cmp(&y.1));
        for (j, d) in near.into_iter().take(8) {
            let q = a.snap.pois[j].clone();
            let dir = fmt::compass(bearing(p.poi.lon, p.poi.lat, q.poi.lon, q.poi.lat));
            if row(ui, &q.poi.name, &fmt::ele(q.ele as f64), &format!("{} {}", fmt::dist(d), dir)) {
                a.selection = Selection::Poi(j);
                a.fly_to(q.poi.lon, q.poi.lat, None);
            }
        }
    }
}

fn point_info(a: &mut Atlas, ui: &mut egui::Ui, lon: f64, lat: f64) {
    close_row(a, ui, "Punkt terenu");
    let e = a.heights.sample_cached(lon, lat).unwrap_or(0.0) as f64;
    ui.heading(fmt::ele(e));
    let g = |dx: f64, dy: f64| a.heights.sample_cached(lon + dx, lat + dy).map(|v| v as f64).unwrap_or(e);
    let kx = 111_320.0 * lat.to_radians().cos();
    let st = 15.0;
    let gx = (g(st / kx, 0.0) - g(-st / kx, 0.0)) / (2.0 * st);
    let gy = (g(0.0, st / 110_574.0) - g(0.0, -st / 110_574.0)) / (2.0 * st);
    let slope = gx.hypot(gy).atan().to_degrees();
    let aspect = (-gx).atan2(-gy).to_degrees().rem_euclid(360.0);
    grid2(ui, vec![
        ("Nachylenie", format!("{}°", fmt::dec(slope, 0))),
        ("Ekspozycja", format!("{} ({}°)", fmt::compass(aspect), aspect.round())),
        ("Szerokość", format!("{lat:.5}°")),
        ("Długość", format!("{lon:.5}°")),
    ]);
    ui.label(RichText::new(fmt::coords(lon, lat)).monospace().small().color(theme::PAPER_3));
    if let Some(n) = a.name_near(lon, lat, 400.0) {
        ui.label(RichText::new(format!("W pobliżu: {n}")).color(theme::PAPER_2));
    }
    point_query_ui(a, ui, lon, lat);
    ui.add_space(6.0);
    point_buttons(a, ui, lon, lat);
}

/// Dane o miejscu z usług: wysokość NMT, adres, działka i atrybuty włączonych warstw.
fn point_query_ui(a: &mut Atlas, ui: &mut egui::Ui, lon: f64, lat: f64) {
    let active: Vec<u16> = a.active_layers().into_iter().map(|l| l.0).collect();
    let key = (lon, lat, active);
    if a.point_query.as_ref().is_none_or(|q| q.0 != key) {
        let info = crate::query::start(lon, lat, &key.2, ui.ctx().clone());
        a.point_query = Some((key, info));
    }
    let Some((_, info)) = &a.point_query else { return };
    let info = info.lock().unwrap();
    ui.add_space(6.0);
    if let Some(ad) = &info.address {
        ui.label(RichText::new(ad).size(17.0).strong());
    }
    if let Some(h) = info.height {
        ui.label(RichText::new(format!("Wysokość z NMT GUGiK: {}", fmt::dec(h as f64, 1))).color(theme::PAPER_2));
    }
    let pairs = |ui: &mut egui::Ui, list: &crate::query::Pairs| {
        for (k, v) in list {
            ui.horizontal_wrapped(|ui| {
                ui.label(RichText::new(k).small().color(theme::PAPER_3));
                ui.label(v);
            });
        }
    };
    if let Some(p) = &info.parcel {
        ui.add_space(6.0);
        ui.label(theme::kicker("Działka"));
        pairs(ui, p);
    }
    for (name, p) in &info.layers {
        ui.add_space(6.0);
        ui.label(theme::kicker(name));
        pairs(ui, p);
    }
    if info.pending > 0 {
        ui.horizontal(|ui| {
            ui.spinner();
            ui.label(RichText::new(format!("pytam usługi · {}", info.pending)).small().color(theme::PAPER_3));
        });
    } else if !key_has_layers(&a.point_query) {
        ui.label(RichText::new("Włącz warstwy, żeby zobaczyć także ich dane o tym miejscu.").small().color(theme::PAPER_3));
    }
}

fn key_has_layers(q: &Option<((f64, f64, Vec<u16>), std::sync::Arc<std::sync::Mutex<crate::query::PointInfo>>)>) -> bool {
    q.as_ref().is_some_and(|q| !q.0 .2.is_empty())
}

fn trail_info(a: &mut Atlas, ui: &mut egui::Ui, e: u32) {
    let snap = a.snap.clone();
    let Some(ed) = snap.graph.edges.get(e as usize) else {
        a.selection = Selection::None;
        return;
    };
    close_row(a, ui, "Szlak turystyczny");
    let routes: Vec<crate::osm::RouteInfo> = ed.routes.iter().filter_map(|&r| snap.graph.routes.get(r as usize).cloned()).collect();
    let title = routes.first().map(|r| r.name.clone()).unwrap_or_else(|| if ed.name.is_empty() { "Odcinek szlaku".into() } else { ed.name.clone() });
    ui.heading(title);
    for r in &routes {
        ui.horizontal(|ui| {
            let c = crate::metrics::TrailColor::parse(&r.color).rgb();
            let (rect, _) = ui.allocate_exact_size(egui::vec2(26.0, 7.0), egui::Sense::hover());
            ui.painter().rect_filled(rect, 3.0, Color32::from_rgb(c[0], c[1], c[2]));
            ui.label(RichText::new(&r.name).strong());
            if !r.r#ref.is_empty() {
                ui.label(RichText::new(&r.r#ref).small().color(theme::PAPER_3));
            }
            if ui.small_button("OSM").clicked() {
                let _ = open::that(format!("https://www.openstreetmap.org/relation/{}", r.id));
            }
        });
    }
    grid2(ui, vec![
        ("Odcinek", fmt::dist(ed.len2)),
        ("Po terenie", fmt::dist(ed.len3)),
        ("Podejścia", fmt::ele(ed.up)),
        ("Zejścia", fmt::ele(ed.down)),
        ("Skala SAC", SAC_LABEL[ed.sac.min(6) as usize].to_string()),
        ("Czas (tam)", fmt::time(metrics::walk_time(ed.len2, ed.up, ed.down) * metrics::sac_factor(ed.sac))),
    ]);
    let route_ids = ed.routes.clone();
    ui.horizontal(|ui| {
        if ui.button("Pokaż cały szlak").clicked() {
            let edges: Vec<u32> = a
                .snap
                .graph
                .edges
                .iter()
                .enumerate()
                .filter(|(_, x)| x.routes.iter().any(|r| route_ids.contains(r)))
                .map(|(i, _)| i as u32)
                .collect();
            if let Some(b) = crate::ui::search::edges_bbox(a, &edges) {
                a.fit_bbox(&b);
            }
        }
    });
}

fn measure_info(a: &mut Atlas, ui: &mut egui::Ui) {
    ui.label(theme::kicker("Pomiar i linia wzroku"));
    ui.heading("Odległość po terenie");
    if a.measure.is_empty() {
        theme::para(ui, "Klikaj na mapie, żeby dodać punkty pomiaru. Dla dwóch punktów program sprawdzi też, czy jeden widać z drugiego.");
        return;
    }
    ui.horizontal(|ui| {
        ui.label(theme::kicker(&format!("Pomiar · {} pkt", a.measure.len())));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui.small_button("Wyczyść").clicked() {
                a.measure.clear();
                a.los = None;
            }
            if ui.small_button("Cofnij").clicked() {
                a.measure.pop();
                a.los = None;
            }
        });
    });
    if a.measure.is_empty() {
        return;
    }
    let (mut d2, mut d3) = (0.0, 0.0);
    for w in a.measure.windows(2) {
        let n = (haversine(w[0].0, w[0].1, w[1].0, w[1].1) / 20.0).ceil().clamp(2.0, 2000.0) as usize;
        let mut prev = (w[0].0, w[0].1, a.heights.sample_cached(w[0].0, w[0].1).unwrap_or(w[0].2) as f64);
        for k in 1..=n {
            let f = k as f64 / n as f64;
            let lon = w[0].0 + (w[1].0 - w[0].0) * f;
            let lat = w[0].1 + (w[1].1 - w[0].1) * f;
            let e = a.heights.sample_cached(lon, lat).unwrap_or(0.0) as f64;
            let h = haversine(prev.0, prev.1, lon, lat);
            d2 += h;
            d3 += h.hypot(e - prev.2);
            prev = (lon, lat, e);
        }
    }
    let first = a.measure[0];
    let last = *a.measure.last().unwrap();
    big(ui, fmt::dist(d3), "po powierzchni", theme::PAPER);
    grid2(ui, vec![
        ("W rzucie", fmt::dist(d2)),
        ("Różnica wysokości", fmt::signed((last.2 - first.2) as f64, "m")),
        ("Azymut", format!("{}° {}", bearing(first.0, first.1, last.0, last.1).round(), fmt::compass(bearing(first.0, first.1, last.0, last.1)))),
        ("Linia prosta", fmt::dist(haversine(first.0, first.1, last.0, last.1))),
    ]);
    if a.measure.len() == 2 {
        ui.add_space(8.0);
        ui.label(theme::kicker("Linia wzroku (oczy 1,7 m, krzywizna Ziemi)"));
        match a.los {
            Some((None, _)) => {
                big(ui, "Widać".into(), "", theme::OK);
                theme::para(ui, "Z punktu 1 widać punkt 2 – teren nie przesłania linii wzroku.");
            }
            Some((Some(f), d)) => {
                big(ui, "Nie widać".into(), "", theme::SIGNAL);
                theme::para(ui, format!("Teren przesłania linię wzroku po {} od punktu 1.", fmt::dist(f * d)));
            }
            None => {
                ui.spinner();
            }
        }
    }
}

fn lens_info(a: &mut Atlas, ui: &mut egui::Ui) {
    let Some((lon, lat, e)) = a.lens_pin else { return };
    let mut unpin = false;
    match a.lens {
        Lens::Vis => {
            ui.label(theme::kicker("Analiza widoczności"));
            ui.heading("Co widać stąd?");
            ui.label(RichText::new(format!("{} · {}", fmt::decimal(lon, lat), fmt::ele(e as f64))).monospace().small().color(theme::PAPER_3));
            ui.label(theme::kicker("Wysokość obserwatora nad terenem"));
            let before = a.prefs.vs_eye;
            ui.horizontal(|ui| {
                for v in [1.7f32, 10.0, 30.0, 100.0] {
                    ui.selectable_value(&mut a.prefs.vs_eye, v, format!("{} m", fmt::dec(v as f64, if v < 2.0 { 1 } else { 0 })));
                }
            });
            if before != a.prefs.vs_eye {
                a.start_viewshed(lon, lat);
            }
            if a.vs_busy {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(RichText::new("liczę promienie widzenia…").color(theme::PAPER_3));
                });
            }
            if let Some(v) = &a.vs {
                big(ui, fmt::dec(v.area_km2, 1), "km² widocznych", theme::TEAL);
                theme::para(ui, format!("w promieniu 32 km · uwzględnia krzywiznę Ziemi i refrakcję · oko {} m n.p.m.", fmt::int(v.observer as f64)));
                ui.add_space(6.0);
                ui.label(theme::kicker(&format!("Widoczne szczyty i schroniska ({})", v.visible.len())));
                let list: Vec<(usize, f64)> = v.visible.iter().take(40).copied().collect();
                for (i, d) in list {
                    let Some(p) = a.snap.pois.get(i).cloned() else { continue };
                    if row(ui, &p.poi.name, &fmt::ele(p.ele as f64), &fmt::dist(d)) {
                        a.selection = Selection::Poi(i);
                    }
                }
            }
        }
        Lens::Rel => {
            ui.label(theme::kicker("Wysokość względna"));
            ui.heading("Co jest wyżej?");
            theme::para(ui, format!("Poziom 0: {} · {}", fmt::ele(e as f64), fmt::decimal(lon, lat)));
            ui.add(egui::Slider::new(&mut a.prefs.rel_range, 50.0..=1500.0).text("zakres barw (m)").logarithmic(true));
            if let Some(r) = &a.rel {
                grid2(ui, vec![("Wyżej (5 km)", format!("{}%", fmt::dec(r.above * 100.0, 0))), ("Niżej (5 km)", format!("{}%", fmt::dec(r.below * 100.0, 0)))]);
            }
            ui.add_space(6.0);
            ui.label(theme::kicker("Szczyty względem punktu"));
            let mut peaks: Vec<(usize, f32, f64)> = a
                .snap
                .pois
                .iter()
                .enumerate()
                .filter(|(_, p)| p.poi.kind == PoiKind::Peak)
                .map(|(i, p)| (i, p.ele - e, haversine(lon, lat, p.poi.lon, p.poi.lat)))
                .filter(|x| x.2 < 15_000.0 && x.2 > 30.0)
                .collect();
            peaks.sort_by(|x, y| x.2.total_cmp(&y.2));
            for (i, dh, d) in peaks.into_iter().take(16) {
                let p = a.snap.pois[i].clone();
                let col = if dh > 0.0 { Color32::from_rgb(255, 176, 140) } else { Color32::from_rgb(143, 211, 255) };
                let r = ui
                    .horizontal(|ui| {
                        let l = ui.add(egui::Label::new(RichText::new(&p.poi.name).strong()).sense(egui::Sense::click()));
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            ui.label(RichText::new(fmt::dist(d)).small().color(theme::PAPER_3));
                            ui.label(RichText::new(fmt::signed(dh as f64, "m")).monospace().color(col));
                        });
                        l.clicked()
                    })
                    .inner;
                if r {
                    a.selection = Selection::Poi(i);
                }
            }
        }
        Lens::Band | Lens::None => {
            ui.label(theme::kicker(if a.prefs.band_tol > 0.0 { "Ta sama wysokość" } else { "Poziomica przez punkt" }));
            ui.heading(fmt::ele(e as f64));
            ui.add(egui::Slider::new(&mut a.prefs.band_tol, 0.0..=50.0).text("pas ± m (0 – sama poziomica)"));
            let tol = a.prefs.band_tol.max(15.0);
            ui.label(theme::kicker(&format!("Na tej samej wysokości (±{} m)", tol.round())));
            let mut same: Vec<(usize, f64)> = a
                .snap
                .pois
                .iter()
                .enumerate()
                .filter(|(_, p)| matches!(p.poi.kind, PoiKind::Peak | PoiKind::Saddle | PoiKind::Hut | PoiKind::Shelter | PoiKind::Lake) && (p.ele - e).abs() <= tol)
                .map(|(i, p)| (i, haversine(lon, lat, p.poi.lon, p.poi.lat)))
                .filter(|x| x.1 > 30.0)
                .collect();
            same.sort_by(|x, y| x.1.total_cmp(&y.1));
            for (i, d) in same.into_iter().take(20) {
                let p = a.snap.pois[i].clone();
                if row(ui, &p.poi.name, &fmt::ele(p.ele as f64), &fmt::dist(d)) {
                    a.selection = Selection::Poi(i);
                }
            }
        }
    }
    ui.add_space(8.0);
    if ui.button("Odepnij soczewkę (Esc)").clicked() {
        unpin = true;
    }
    if unpin {
        a.unpin_lens();
    }
}

/// Styl mapy, warstwy i słońce jako rozwijane menu na górnym pasku.
pub fn view_menus(a: &mut Atlas, ui: &mut egui::Ui) {
    let menu = |ui: &mut egui::Ui, a: &mut Atlas, glyph: &str, label: &str, body: fn(&mut Atlas, &mut egui::Ui)| {
        ui.menu_button(format!("{glyph}  {label}"), |ui| {
            ui.set_min_width(320.0);
            body(a, ui);
        });
    };
    let style = a.prefs.style.label();
    menu(ui, a, theme::icon::PALETTE, style, style_tab);
    menu(ui, a, theme::icon::STACK, "Widok", layers_tab);
    menu(ui, a, theme::icon::SUN, "Słońce", light_tab);
}

fn style_tab(a: &mut Atlas, ui: &mut egui::Ui) {
    for st in Style::ALL {
        let glyph = match st {
            Style::Satellite => theme::icon::GLOBE_HEMISPHERE_EAST,
            Style::Terrain => theme::icon::MOUNTAINS,
            Style::Map => theme::icon::MAP_TRIFOLD,
            Style::Hypso => theme::icon::STACK,
            Style::Slope => theme::icon::ANGLE,
            Style::Aspect => theme::icon::COMPASS,
        };
        if theme::row_button(ui, glyph, st.label(), a.prefs.style == st).clicked() {
            a.prefs.style = st;
        }
    }
    let legend = match a.prefs.style {
        Style::Slope => Some("Klasy nachylenia jak na mapach lawinowych: 15–25° · 25–30° · 30–35° · 35–40° · 40–45° · ponad 45°."),
        Style::Aspect => Some("Barwa – kierunek, w który opada stok; nasycenie – nachylenie."),
        Style::Hypso => Some("Barwy wysokości: niziny zielone, piętro hal brązowe, najwyżej biel."),
        Style::Satellite => Some("Sentinel-2 cloudless 2020 (EOX) – 10 m; z bliska ortofotomapa GUGiK."),
        _ => None,
    };
    if let Some(l) = legend {
        theme::para(ui, l);
    }
    ui.add_space(10.0);
    ui.add(egui::Slider::new(&mut a.prefs.exag, 1.0..=3.0).step_by(0.1).text("przewyższenie ×"));
}

fn layers_tab(a: &mut Atlas, ui: &mut egui::Ui) {
    theme::switch(ui, &mut a.prefs.labels, "Nazwy miejsc (L)");
    theme::switch(ui, &mut a.prefs.trails, "Szlaki (T)");
    theme::switch(ui, &mut a.prefs.contours, "Poziomice (C)");
    theme::switch(ui, &mut a.prefs.km_grid, "Siatka kilometrowa");
    theme::switch(ui, &mut a.prefs.shadows, "Cienie rzeźby");
    theme::switch(ui, &mut a.prefs.detail, "Drobna rzeźba (mapa normalnych)");
    theme::switch(ui, &mut a.prefs.snow, "Śnieg");
    theme::switch(ui, &mut a.prefs.ortho, "Ortofoto GUGiK");
    theme::switch(ui, &mut a.prefs.landcover, "Pokrycie terenu z OSM");
    ui.add_space(10.0);
    ui.add(egui::Slider::new(&mut a.prefs.label_density, 0.3..=2.0).text("gęstość nazw"));
}

fn light_tab(a: &mut Atlas, ui: &mut egui::Ui) {
    let (day, hour) = a.when();
    let mut h = hour;
    let mut d = day;
    ui.horizontal(|ui| {
        if ui.small_button(theme::icon::CARET_LEFT).on_hover_text("dzień wcześniej").clicked() {
            d = d.add_days(-1);
        }
        ui.label(RichText::new(d.label()).strong());
        if ui.small_button(theme::icon::CARET_RIGHT).on_hover_text("dzień później").clicked() {
            d = d.add_days(1);
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui.small_button("+1 mies.").clicked() {
                d = d.add_days(30);
            }
            if ui.small_button("−1 mies.").clicked() {
                d = d.add_days(-30);
            }
        });
    });
    let r = ui.add(egui::Slider::new(&mut h, 0.0..=23.99).text("godzina").custom_formatter(|v, _| fmt::hour(v)));
    if r.changed() || d != day {
        a.prefs.custom_time = Some((d.y, d.m, d.d, h));
    }
    let t = sun::times(d, a.cam.lat, a.cam.lon);
    let (az, alt) = a.sun();
    ui.add_space(6.0);
    grid2(ui, vec![
        ("Wschód", fmt::hour(t.rise)),
        ("Zachód", fmt::hour(t.set)),
        ("Słońce", format!("{}° nad horyzontem", fmt::dec(alt, 0))),
        ("Azymut", format!("{}° {}", az.round(), fmt::compass(az))),
        ("Granica śniegu", fmt::ele(sun::snowline(d))),
        ("Górowanie", format!("{} · {}°", fmt::hour(t.noon), t.max_alt.round())),
    ]);
    ui.add_space(6.0);
    if theme::row_button(ui, theme::icon::CLOCK, "Teraz", a.prefs.custom_time.is_none()).clicked() {
        a.prefs.custom_time = None;
    }
    if theme::row_button(ui, theme::icon::SUN, "Złota godzina", false).on_hover_text("godzina przed zachodem").clicked() {
        a.prefs.custom_time = Some((d.y, d.m, d.d, (t.set - 1.0).max(0.0)));
    }
    if theme::row_button(ui, theme::icon::SUN_HORIZON, "Wschód słońca", false).clicked() {
        a.prefs.custom_time = Some((d.y, d.m, d.d, t.rise + 0.25));
    }
    if theme::row_button(ui, theme::icon::SUN_DIM, "Południe", false).clicked() {
        a.prefs.custom_time = Some((d.y, d.m, d.d, t.noon));
    }
    let _ = Day::today;
}

/// Strona „Grafika” w ustawieniach.
pub fn graphics(a: &mut Atlas, ui: &mut egui::Ui) {
    ui.horizontal(|ui| {
        ui.label(theme::kicker(&format!("Jakość · {}", a.prefs.quality.label())));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.label(RichText::new(format!("{:.0} kl./s · {:.1} ms", a.fps(), 1000.0 / a.fps().max(1.0))).small().color(theme::TEAL));
        });
    });
    ui.horizontal(|ui| {
        for q in Quality::PRESETS {
            if ui.selectable_label(a.prefs.quality == q, q.label()).clicked() {
                a.prefs.quality = q;
                let (s, d, md, mi) = q.values();
                a.prefs.render_scale = s;
                a.prefs.detail_px = d;
                a.prefs.max_dem = md;
                a.prefs.max_img = mi;
            }
        }
    });
    ui.add_space(6.0);
    let mut changed = false;
    changed |= ui.add(egui::Slider::new(&mut a.prefs.render_scale, 0.25..=1.0).text("rozdzielczość renderu").custom_formatter(|v, _| format!("{:.0}%", v * 100.0))).changed();
    let mut detail = 700.0 - a.prefs.detail_px;
    if ui.add(egui::Slider::new(&mut detail, 100.0..=520.0).text("szczegółowość terenu").show_value(false)).changed() {
        a.prefs.detail_px = 700.0 - detail;
        changed = true;
    }
    changed |= ui.add(egui::Slider::new(&mut a.prefs.max_dem, 150..=1200).text("kafle terenu w pamięci GPU")).changed();
    changed |= ui.add(egui::Slider::new(&mut a.prefs.max_img, 150..=1500).text("kafle zdjęć w pamięci GPU")).changed();
    if changed {
        a.prefs.quality = Quality::Custom;
    }
    if let Some(r) = &a.renderer {
        let (d, i) = r.tiles.resident();
        ui.label(RichText::new(format!("Rysowane kafle: {} · w pamięci: teren {d}, zdjęcia {i}", r.drawn)).small().monospace().color(theme::PAPER_3));
    }
}

pub fn help(a: &mut Atlas, ctx: &egui::Context) {
    if !a.help {
        return;
    }
    let rows = [
        ("1  2", "eksploracja · pomiar"),
        ("H  W  O", "soczewka: ta sama wysokość · względna · widoczność"),
        ("/", "wyszukiwarka"),
        ("M", "następny styl mapy"),
        ("P", "pokaż / ukryj panel szczegółów"),
        ("T  L  C", "szlaki · nazwy · poziomice"),
        ("klik z soczewką / Esc", "przypnij / odepnij soczewkę"),
        ("+  −", "przewyższenie terenu"),
        ("[  ]", "pora dnia −/+ 30 min"),
        ("N", "północ u góry"),
        ("V", "widok z góry / ukośny"),
        ("R", "widok początkowy"),
        ("S", "zrzut ekranu PNG"),
        ("⌫", "usuń ostatni punkt pomiaru"),
        ("Esc", "anuluj / wyjdź"),
        ("strzałki · Q E · PgUp PgDn", "przesuwanie · obrót · nachylenie"),
        ("lewy przycisk", "przesuwanie (chwyć teren)"),
        ("prawy przycisk", "obrót i nachylenie · klik – menu analiz"),
        ("kółko", "przybliżanie do kursora"),
        ("dwuklik", "przybliż do punktu"),
        ("Ctrl+,", "ustawienia"),
    ];
    let mut open = true;
    egui::Window::new("Jak korzystać z programu")
        .collapsible(false)
        .resizable(false)
        .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
        .open(&mut open)
        .show(ctx, |ui| {
            ui.set_width(560.0);
            theme::para(ui, "Atlas łączy rzeczywisty model terenu Polski z danymi OSM. Wszystkie obliczenia – widoczność, cienie – odbywają się lokalnie na tym komputerze.");
            ui.add_space(8.0);
            egui::Grid::new("skroty").num_columns(2).striped(true).spacing([24.0, 6.0]).show(ui, |ui| {
                for (k, v) in rows {
                    ui.label(v);
                    ui.label(RichText::new(k).monospace().color(theme::BRASS));
                    ui.end_row();
                }
            });
        });
    if !open {
        a.help = false;
    }
}
