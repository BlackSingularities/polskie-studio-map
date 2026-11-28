//! Zakładka „Warstwy” podzielona na dwie części:
//! - **Globalne** – wszystko, co barwi całą mapę: styl podkładu, składniki terenu (ortofoto, cienie,
//!   rzeźba, pokrycie, śnieg, NMPT), pola pogody, obraz satelitarny, kartogram GUS i warstwy
//!   obszarowe z katalogu (cieniowanie, hipsometria, geologia, użytki, strefy planów…);
//! - **Obiekty** – rzeczy na mapie: nazwy, szlaki, poziomice, siatka, dane na żywo i warstwy
//!   obiektowe z katalogu (granice, działki, budynki, sieci, formy ochrony przyrody, maszty…).
//!
//! Globalne leżą zawsze pod obiektami; w każdej części kolejność ustawia się strzałkami.

use eframe::egui::{self, Color32, RichText};

use crate::layers::{self, catalog, Kind};
use crate::ui::atlas::{Atlas, BaseLayer, Style};
use crate::ui::panels::{style_glyph, toggle_tab, Tab};
use crate::ui::search::fold;
use crate::ui::theme;

pub fn body(a: &mut Atlas, ui: &mut egui::Ui) {
    let kinds = Kind::ALL;
    let cur = kinds.iter().position(|k| *k == a.prefs.layer_kind).unwrap_or(0);
    let items = [
        (theme::icon::GLOBE_HEMISPHERE_EAST, Kind::Global.label(), a.count_on(Kind::Global)),
        (theme::icon::MAP_PIN, Kind::Objects.label(), a.count_on(Kind::Objects)),
    ];
    if let Some(i) = theme::segmented(ui, &items, cur) {
        a.prefs.layer_kind = kinds[i];
    }
    let kind = a.prefs.layer_kind;
    ui.add_space(2.0);
    theme::para(ui, kind.desc());
    ui.add_space(4.0);
    ui.add(
        egui::TextEdit::singleline(&mut a.layer_query)
            .hint_text(format!("Szukaj wśród {} warstw (obu rodzajów)…", catalog().layers.len()))
            .desired_width(f32::INFINITY),
    );
    let q = fold(a.layer_query.trim());
    if !q.is_empty() {
        search_results(a, ui, &q);
        return;
    }
    presets(a, ui);
    match kind {
        Kind::Global => global(a, ui),
        Kind::Objects => objects(a, ui),
    }
    ui.add_space(10.0);
    theme::switch(ui, &mut a.prefs.legend, "Legenda na mapie (J)");
}

/// Styl podkładu jako kafelki (też w menu „Styl mapy” na pasku).
pub fn style_tiles(a: &mut Atlas, ui: &mut egui::Ui) {
    theme::tile_grid(ui, 3, |ui, w| {
        for st in Style::ALL {
            if theme::tile(ui, style_glyph(st), st.label(), a.prefs.style == st, w).clicked() {
                a.prefs.style = st;
            }
        }
    });
    let note = match a.prefs.style {
        Style::Slope => Some("Klasy nachylenia jak na mapach lawinowych: 15–25° · 25–30° · 30–35° · 35–40° · 40–45° · ponad 45°."),
        Style::Aspect => Some("Barwa – kierunek, w który opada stok; nasycenie – nachylenie."),
        Style::Hypso => Some("Barwy wysokości: niziny zielone, wyżyny żółte i brązowe, najwyżej biel."),
        Style::Satellite => Some("Sentinel-2 cloudless 2020 (EOX) – 10 m; z bliska ortofotomapa GUGiK."),
        Style::Map => Some("Jasna mapa z cieniowaniem, wodami i lasami z OSM."),
        Style::Terrain => Some("Barwy terenu zgodne z porą roku, śnieg i cienie zgodne z porą dnia."),
    };
    if let Some(n) = note {
        theme::para(ui, n);
    }
    ui.add(egui::Slider::new(&mut a.prefs.exag, 1.0..=3.0).step_by(0.1).text("przewyższenie ×"));
}

/// Wbudowane składniki mapy jednego rodzaju jako kafelki.
pub fn base_tiles(a: &mut Atlas, ui: &mut egui::Ui, kind: Kind, cols: usize) {
    let list: Vec<BaseLayer> = BaseLayer::ALL.iter().copied().filter(|b| b.kind() == kind).collect();
    let mut flip = None;
    theme::tile_grid(ui, cols, |ui, w| {
        for b in &list {
            let (g, t, keys) = b.info();
            let off = b.off_reason(&a.prefs);
            let on = b.on(&a.prefs) && off.is_none();
            let label = match keys {
                Some(k) => format!("{t} ({})", k.text()),
                None => t.to_string(),
            };
            let r = theme::tile_ex(ui, g, &label, on, w, off);
            if r.clicked() {
                flip = Some(*b);
            }
            r.on_hover_text(b.hint());
        }
    });
    if let Some(b) = flip {
        let on = !b.on(&a.prefs);
        a.set_base(b, on);
    }
}

fn global(a: &mut Atlas, ui: &mut egui::Ui) {
    theme::section(ui, theme::icon::PAINT_BRUSH, "Podkład", a.prefs.style.label());
    style_tiles(a, ui);
    theme::section(ui, theme::icon::MOUNTAINS, "Teren", "");
    base_tiles(a, ui, Kind::Global, 3);
    theme::section(ui, theme::icon::CLOUD_SUN, "Pogoda i statystyka", "");
    extras_global(a, ui);
    active_stack(a, ui, Kind::Global);
    catalog_list(a, ui, Kind::Global);
}

fn objects(a: &mut Atlas, ui: &mut egui::Ui) {
    theme::section(ui, theme::icon::MAP_PIN, "Na mapie", "");
    base_tiles(a, ui, Kind::Objects, 4);
    ui.add(egui::Slider::new(&mut a.prefs.label_density, 0.3..=2.0).text("gęstość nazw"));
    theme::section(ui, theme::icon::BROADCAST, "Na żywo", "odświeżane samoczynnie");
    feeds(a, ui);
    if crate::stats::map().is_some() {
        theme::switch(ui, &mut a.stats_bars, "Słupki 3D kartogramu GUS");
    }
    theme::switch(ui, &mut a.wind_particles, "Cząstki wiatru (Open-Meteo)");
    active_stack(a, ui, Kind::Objects);
    catalog_list(a, ui, Kind::Objects);
}

/// Pole pogody, obraz satelitarny i kartogram GUS – warstwy globalne spoza katalogu.
fn extras_global(a: &mut Atlas, ui: &mut egui::Ui) {
    let mut open_tab = None;
    // pole pogody
    let weather = a.weather_field.map(|f| {
        let when = match (crate::weather::grid(), a.weather_hour) {
            (Some(g), Some(h)) => crate::weather::time_label(g.times[h.min(g.times.len() - 1)]),
            _ => "teraz".to_string(),
        };
        format!("{} · {when}", f.label())
    });
    if extra_row(ui, theme::icon::THERMOMETER, "Pole pogody", weather.as_deref(), "Pogoda (K)") {
        if weather.is_some() {
            a.weather_field = None;
        } else {
            open_tab = Some(Tab::Weather);
        }
    }
    theme::switch(ui, &mut a.satellite, "Obraz satelitarny IR (EUMETSAT, co 15 min)");
    // kartogram
    let gus = crate::stats::map().map(|m| format!("{} · {} · {}", m.label, m.level.label(), m.year));
    if extra_row(ui, theme::icon::CHART_BAR, "Kartogram GUS", gus.as_deref(), "Statystyka (B)") {
        if gus.is_some() {
            crate::stats::clear_map();
        } else {
            open_tab = Some(Tab::Stats);
        }
    }
    if let Some(t) = open_tab {
        toggle_tab(a, t);
        a.prefs.panel_open = true;
    }
}

/// Wiersz dodatkowej warstwy: tytuł, stan; przycisk wyłącza ją albo prowadzi do zakładki.
fn extra_row(ui: &mut egui::Ui, glyph: &str, title: &str, state: Option<&str>, go: &str) -> bool {
    let mut clicked = false;
    theme::card(state.is_some()).inner_margin(egui::Margin::symmetric(10, 7)).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.horizontal(|ui| {
            ui.label(RichText::new(glyph).size(17.0).color(if state.is_some() { theme::BRASS } else { theme::PAPER_3 }));
            ui.vertical(|ui| {
                ui.label(RichText::new(title).strong());
                ui.label(RichText::new(state.unwrap_or("wyłączone")).small().color(theme::PAPER_3));
            });
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let b = if state.is_some() { ui.small_button(theme::icon::X).on_hover_text("Wyłącz") } else { ui.small_button(go) };
                clicked = b.clicked();
            });
        });
    });
    clicked
}

/// Zestawy warstw: wbudowane i własne (obu rodzajów naraz).
fn presets(a: &mut Atlas, ui: &mut egui::Ui) {
    egui::CollapsingHeader::new(RichText::new(theme::ic(theme::icon::STACK, "Zestawy warstw")).strong())
        .id_salt("zestawy")
        .default_open(false)
        .show(ui, |ui| {
            let builtin = catalog().presets.clone();
            let mut pick = None;
            let mut drop = None;
            ui.horizontal_wrapped(|ui| {
                for p in &builtin {
                    if ui.button(&p.name).on_hover_text(preset_tip(&p.layers)).clicked() {
                        pick = Some(p.layers.clone());
                    }
                }
                for p in &a.user_presets {
                    let r = ui.button(theme::ic(theme::icon::USER, &p.name)).on_hover_text(format!("{}\nprawy przycisk – usuń zestaw", preset_tip(&p.layers)));
                    if r.clicked() {
                        pick = Some(p.layers.clone());
                    }
                    if r.secondary_clicked() {
                        drop = Some(p.name.clone());
                    }
                }
            });
            if let Some(ids) = pick {
                for l in &mut a.prefs.layers {
                    l.on = ids.contains(&l.id);
                }
            }
            if let Some(name) = drop {
                a.user_presets.retain(|p| p.name != name);
                if let Err(e) = layers::save_presets(&a.user_presets) {
                    a.notify(format!("Nie udało się zapisać zestawów: {e}"));
                }
            }
            ui.horizontal(|ui| {
                ui.add(egui::TextEdit::singleline(&mut a.new_preset).hint_text("Nazwa własnego zestawu").desired_width(200.0));
                let any = a.prefs.layers.iter().any(|l| l.on);
                if ui.add_enabled(any && !a.new_preset.trim().is_empty(), egui::Button::new("Zapisz włączone")).clicked() {
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
            theme::para(ui, "Zestaw zapamiętuje warstwy z katalogu. Cały widok – z kamerą, stylem i danymi na żywo – zapiszesz w bibliotece (Ctrl+D).");
        });
}

fn preset_tip(ids: &[String]) -> String {
    ids.iter()
        .filter_map(|id| catalog().layers.iter().find(|d| &d.id == id))
        .map(|d| format!("{} · {}", d.name, d.kind.label().to_lowercase()))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Dane na żywo: przełączniki, stan pobierania i ostrzeżenia.
fn feeds(a: &mut Atlas, ui: &mut egui::Ui) {
    for def in &crate::feeds::FEEDS {
        let mut on = a.prefs.feeds.iter().any(|f| f == def.id);
        if theme::switch(ui, &mut on, def.name).on_hover_text(format!("Źródło: {}", def.attribution)).changed() {
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
            (format!("{} na mapie · {} min temu", st.count, st.age_min.unwrap_or(0)), theme::PAPER_3)
        };
        ui.horizontal(|ui| {
            ui.add_space(10.0);
            ui.label(RichText::new(t).small().color(c));
        });
        for (title, body) in a.feeds.warnings(def.id).iter() {
            egui::CollapsingHeader::new(RichText::new(title).small()).show(ui, |ui| {
                ui.label(RichText::new(body).small());
            });
        }
    }
}

/// Włączone warstwy z katalogu jednego rodzaju, od góry stosu: krycie, kolejność, stan i legenda.
fn active_stack(a: &mut Atlas, ui: &mut egui::Ui, kind: Kind) {
    let on: Vec<usize> = (0..a.prefs.layers.len())
        .rev()
        .filter(|&i| a.prefs.layers[i].on && layers::index_of(&a.prefs.layers[i].id).is_some_and(|ci| layers::kind_of(ci) == kind))
        .collect();
    let note = if on.is_empty() { "brak – włącz w katalogu poniżej".to_string() } else { "od góry stosu".to_string() };
    theme::section(ui, theme::icon::STACK, &format!("Włączone z katalogu ({})", on.len()), &note);
    if on.is_empty() {
        return;
    }
    let (loading, drawn) = match &a.renderer {
        Some(r) => (r.tiles.layer_loading.clone(), r.tiles.layer_drawn.clone()),
        None => Default::default(),
    };
    let mut swap = None;
    let mut zoom_to = None;
    for (k, &i) in on.iter().enumerate() {
        let Some(ci) = layers::index_of(&a.prefs.layers[i].id) else { continue };
        let d = &catalog().layers[ci as usize];
        theme::card(false).inner_margin(egui::Margin::symmetric(10, 8)).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.vertical(|ui| {
                    ui.add(egui::Label::new(RichText::new(d.name.as_str()).strong()).truncate());
                    ui.label(RichText::new(&d.group).small().color(theme::PAPER_3));
                });
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.small_button(theme::icon::X).on_hover_text("Wyłącz").clicked() {
                        a.prefs.layers[i].on = false;
                    }
                    if ui.add_enabled(k + 1 < on.len(), egui::Button::new(theme::icon::ARROW_DOWN).small()).on_hover_text("Niżej w stosie").clicked() {
                        swap = Some((i, on[k + 1]));
                    }
                    if ui.add_enabled(k > 0, egui::Button::new(theme::icon::ARROW_UP).small()).on_hover_text("Wyżej w stosie").clicked() {
                        swap = Some((i, on[k - 1]));
                    }
                    ui.label(RichText::new(theme::icon::INFO).color(theme::PAPER_3)).on_hover_text(layer_tip(d));
                });
            });
            ui.spacing_mut().slider_width = (ui.available_width() - 70.0).max(60.0);
            ui.add(egui::Slider::new(&mut a.prefs.layers[i].opacity, 0.05..=1.0).custom_formatter(|v, _| format!("{:.0}%", v * 100.0)));
            let status = if let Some(e) = layers::error_of(ci) {
                Some((format!("serwis niedostępny: {e}"), theme::WARN, false))
            } else if loading.get(&ci).copied().unwrap_or(0) > 0 {
                Some((format!("ładowanie · {} kafli", loading[&ci]), theme::PAPER_3, false))
            } else if drawn.get(&ci).copied().unwrap_or(0) == 0 {
                Some((format!("nic w tym widoku – widoczna od poziomu {}", d.min_z), theme::PAPER_3, true))
            } else {
                None
            };
            if let Some((t, c, zoom)) = status {
                ui.horizontal(|ui| {
                    ui.label(RichText::new(t).small().color(c));
                    if zoom && a.cam.distance > a.distance_for_zoom(d.min_z) && ui.small_button("Przybliż").clicked() {
                        zoom_to = Some(d.min_z);
                    }
                });
            }
            let header = egui::CollapsingHeader::new(RichText::new("Legenda").small()).id_salt(("legenda", ci)).show(ui, |ui| match a.legends.get(&ci) {
                Some(Some(tex)) => {
                    let s = tex.size_vec2().min(egui::vec2(360.0, 380.0));
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
        });
        ui.add_space(2.0);
    }
    if let Some((x, y)) = swap {
        a.prefs.layers.swap(x, y);
    }
    if let Some(z) = zoom_to {
        let d = a.distance_for_zoom(z) * 0.8;
        let (lon, lat) = (a.cam.lon, a.cam.lat);
        a.fly_to(lon, lat, Some(d));
    }
    ui.horizontal(|ui| {
        if ui.small_button(theme::ic(theme::icon::EYE_SLASH, "Wyłącz wszystkie")).clicked() {
            for &i in &on {
                a.prefs.layers[i].on = false;
            }
        }
        if ui.small_button(theme::ic(theme::icon::CIRCLE_HALF, "Krycie 50%")).clicked() {
            for &i in &on {
                a.prefs.layers[i].opacity = 0.5;
            }
        }
    });
}

fn layer_tip(d: &layers::LayerDef) -> String {
    let mut s = String::new();
    if !d.desc.is_empty() {
        s.push_str(&d.desc);
        s.push('\n');
    }
    s.push_str(&format!("Rodzaj: {}\nGrupa: {}\nŹródło: {}\nWidoczna od poziomu {}", d.kind.title().to_lowercase(), d.group, d.attribution, d.min_z));
    s
}

/// Katalog warstw jednego rodzaju w grupach.
fn catalog_list(a: &mut Atlas, ui: &mut egui::Ui, kind: Kind) {
    let n = catalog().layers.iter().filter(|d| d.kind == kind).count();
    theme::section(ui, theme::icon::BOOKS, "Katalog", &format!("{n} warstw"));
    let mut groups: Vec<&str> = Vec::new();
    for d in catalog().layers.iter().filter(|d| d.kind == kind) {
        if !groups.contains(&d.group.as_str()) {
            groups.push(&d.group);
        }
    }
    for g in groups {
        let list: Vec<&layers::LayerDef> = catalog().layers.iter().filter(|d| d.group == g && d.kind == kind).collect();
        let on = list.iter().filter(|d| a.prefs.layers.iter().any(|l| l.id == d.id && l.on)).count();
        let title = if on > 0 { format!("{g}  ·  {on}/{}", list.len()) } else { format!("{g}  ({})", list.len()) };
        egui::CollapsingHeader::new(RichText::new(title).color(if on > 0 { theme::BRASS } else { theme::PAPER })).id_salt(("katalog", g, kind)).show(ui, |ui| {
            for d in list {
                layer_check(a, ui, d, false);
            }
        });
    }
}

/// Przełącznik warstwy z katalogu z podpowiedzią (opis, źródło, zakres).
fn layer_check(a: &mut Atlas, ui: &mut egui::Ui, d: &layers::LayerDef, with_kind: bool) {
    let Some(st) = a.prefs.layers.iter_mut().find(|l| l.id == d.id) else { return };
    ui.horizontal(|ui| {
        ui.checkbox(&mut st.on, d.name.as_str()).on_hover_text(layer_tip(d));
        if with_kind {
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let (g, c) = match d.kind {
                    Kind::Global => (theme::icon::GLOBE_HEMISPHERE_EAST, Color32::from_rgb(120, 200, 160)),
                    Kind::Objects => (theme::icon::MAP_PIN, theme::BRASS),
                };
                ui.label(RichText::new(format!("{g} {}", d.kind.label().to_lowercase())).small().color(c));
                ui.label(RichText::new(&d.group).small().color(theme::PAPER_3));
            });
        }
    });
}

/// Wyniki wyszukiwania warstw w całym katalogu (oba rodzaje).
fn search_results(a: &mut Atlas, ui: &mut egui::Ui, q: &str) {
    let hits: Vec<&layers::LayerDef> = catalog()
        .layers
        .iter()
        .filter(|d| fold(&d.name).contains(q) || fold(&d.group).contains(q) || fold(&d.desc).contains(q) || fold(d.kind.label()).contains(q))
        .collect();
    let bases: Vec<BaseLayer> = BaseLayer::ALL.iter().copied().filter(|b| fold(b.info().1).contains(q)).collect();
    theme::section(ui, theme::icon::MAGNIFYING_GLASS, "Wyniki", &format!("{}", hits.len() + bases.len()));
    for b in bases {
        let mut on = b.on(&a.prefs);
        let (g, t, _) = b.info();
        if theme::switch(ui, &mut on, &format!("{g}  {t}")).on_hover_text(b.hint()).changed() {
            a.set_base(b, on);
        }
    }
    for d in hits {
        layer_check(a, ui, d, true);
    }
    if ui.small_button("Wyczyść wyszukiwanie").clicked() {
        a.layer_query.clear();
    }
}
