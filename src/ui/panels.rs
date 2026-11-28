//! Interfejs nad mapą (układ jak w GRANI): górny pasek (panele, narzędzia, wyszukiwarka, pora dnia, styl,
//! widok), prawy panel z zakładkami (szczegóły, warstwy, statystyka, pogoda), sterowanie kamerą i pomoc.

use eframe::egui::{self, Color32, RichText};

use crate::fmt;
use crate::geo::{bearing, haversine};
use crate::metrics::{self, SAC_LABEL};
use crate::osm::PoiKind;
use crate::sun::{self, Day};
use crate::ui::atlas::{Atlas, Lens, Quality, Selection, Style, Tool};
use crate::ui::theme;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Tab {
    Info,
    Layers,
    Stats,
    Weather,
    Library,
}

impl Tab {
    pub const ALL: [Tab; 5] = [Tab::Info, Tab::Layers, Tab::Stats, Tab::Weather, Tab::Library];

    pub fn label(self) -> &'static str {
        match self {
            Tab::Info => "Szczegóły",
            Tab::Layers => "Warstwy",
            Tab::Stats => "Statystyka",
            Tab::Weather => "Pogoda",
            Tab::Library => "Biblioteka",
        }
    }

    pub fn icon(self) -> &'static str {
        match self {
            Tab::Info => theme::icon::COMPASS,
            Tab::Layers => theme::icon::STACK,
            Tab::Stats => theme::icon::CHART_BAR,
            Tab::Weather => theme::icon::CLOUD_SUN,
            Tab::Library => theme::icon::BOOKMARKS,
        }
    }

    pub fn key(self) -> &'static str {
        match self {
            Tab::Info => "P",
            Tab::Layers => "G",
            Tab::Stats => "B",
            Tab::Weather => "K",
            Tab::Library => "Z",
        }
    }

    /// Skrót otwierający zakładkę (szczegóły – przez „P”, pokaż/ukryj panel).
    pub fn keys(self) -> Option<crate::ui::atlas::Keys> {
        use egui::Key as K;
        let key = match self {
            Tab::Info => return None,
            Tab::Layers => K::G,
            Tab::Stats => K::B,
            Tab::Weather => K::K,
            Tab::Library => K::Z,
        };
        Some(crate::ui::atlas::Keys { key, ctrl: false })
    }
}

/// Otwiera panel boczny na zakładce albo go chowa, gdy ta zakładka już jest widoczna.
pub fn toggle_tab(a: &mut Atlas, tab: Tab) {
    a.prefs.panel_open = !(a.prefs.panel_open && a.tab == tab);
    a.tab = tab;
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

/// Górny pasek na całą szerokość okna: od lewej panele i narzędzia, wyszukiwarka na środku,
/// po prawej pora dnia, styl mapy, widok i przyciski programu (`trailing`).
pub fn top_bar(a: &mut Atlas, ctx: &egui::Context, trailing: impl FnOnce(&mut Atlas, &mut egui::Ui)) {
    // zegar przy porze „teraz” ma iść także przy nieruchomym widoku
    ctx.request_repaint_after(std::time::Duration::from_secs(20));
    egui::TopBottomPanel::top("gorny-pasek")
        .exact_height(theme::TOP_BAR_H)
        .show_separator_line(false)
        .frame(egui::Frame::new().fill(theme::INK_1).inner_margin(egui::Margin::symmetric(10, 6)))
        .show(ctx, |ui| {
            let full = ui.max_rect().expand2(egui::vec2(10.0, 6.0));
            ui.painter().line_segment([full.left_bottom(), full.right_bottom()], egui::Stroke::new(1.0, theme::INK_3));
            ui.horizontal_centered(|ui| {
                ui.spacing_mut().item_spacing.x = 3.0;
                tools(a, ui);
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.spacing_mut().item_spacing.x = 4.0;
                    trailing(a, ui);
                    theme::bar_divider(ui);
                    view_button(a, ui);
                    style_button(a, ui);
                    sun_button(a, ui);
                    // wyszukiwarka na środku pozostałego miejsca
                    let free = ui.available_width();
                    let w = (free - 24.0).min(440.0);
                    if w >= 160.0 {
                        ui.add_space(((free - w) / 2.0).max(0.0));
                        search_field(a, ui, w);
                    } else if theme::tool_tile(ui, theme::icon::MAGNIFYING_GLASS, "Szukaj", a.search.is_some(), 52.0).on_hover_text("Szukaj miejsca (/)").clicked() {
                        a.search = Some(Default::default());
                    }
                });
            });
        });
}

/// Pole wyszukiwarki na pasku; kliknięcie otwiera wyszukiwarkę w tym samym miejscu.
fn search_field(a: &mut Atlas, ui: &mut egui::Ui, w: f32) {
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(w, 40.0), egui::Sense::click());
    let p = ui.painter();
    p.rect_filled(rect, 12.0, if resp.hovered() { Color32::from_white_alpha(18) } else { Color32::from_white_alpha(8) });
    p.text(rect.left_center() + egui::vec2(16.0, 0.0), egui::Align2::LEFT_CENTER, theme::icon::MAGNIFYING_GLASS, egui::FontId::proportional(18.0), theme::PAPER_2);
    p.with_clip_rect(rect.shrink(4.0)).text(rect.left_center() + egui::vec2(44.0, 0.0), egui::Align2::LEFT_CENTER, "Miejscowość, adres, działka, szczyt…", egui::FontId::proportional(14.0), theme::PAPER_3);
    p.text(rect.right_center() - egui::vec2(14.0, 0.0), egui::Align2::RIGHT_CENTER, "/", egui::FontId::monospace(12.0), theme::PAPER_3);
    ui.ctx().data_mut(|d| d.insert_temp(egui::Id::new("szukaj-pole-rect"), rect));
    if resp.clicked() {
        a.search = Some(Default::default());
    }
}

/// Lewa część paska: panele (szczegóły, warstwy, statystyka, pogoda), pomiar, soczewki i panorama.
fn tools(a: &mut Atlas, ui: &mut egui::Ui) {
    // węższe kafelki w mniejszym oknie, żeby wyszukiwarka i przyciski po prawej się zmieściły
    #[allow(non_snake_case)]
    let W: f32 = if ui.ctx().screen_rect().width() < 1500.0 { 54.0 } else { 64.0 };
    for tab in Tab::ALL {
        let on = a.prefs.panel_open && a.tab == tab && !(tab == Tab::Info && a.tool == Tool::Measure);
        let n = match tab {
            Tab::Layers => a.count_on(crate::layers::Kind::Global) + a.count_on(crate::layers::Kind::Objects),
            Tab::Library => a.library.views.len() + a.library.places.len(),
            _ => 0,
        };
        let label = if n > 0 { format!("{} · {n}", tab.label()) } else { tab.label().to_string() };
        if theme::tool_tile(ui, tab.icon(), &label, on, W).on_hover_text(format!("{} ({})", tab.label(), tab.key())).clicked() {
            if tab == Tab::Info && a.tool == Tool::Measure {
                a.tool = Tool::Explore;
                a.prefs.panel_open = true;
                a.tab = Tab::Info;
            } else {
                toggle_tab(a, tab);
            }
        }
    }
    theme::bar_divider(ui);
    if theme::tool_tile(ui, theme::icon::RULER, "Pomiar", a.tool == Tool::Measure, W).on_hover_text("Pomiar odległości, pola i linii wzroku (2)").clicked() {
        if a.tool == Tool::Measure {
            a.tool = Tool::Explore;
        } else {
            a.tool = Tool::Measure;
            a.prefs.panel_open = true;
            a.tab = Tab::Info;
        }
    }
    theme::bar_divider(ui);
    let lenses = [
        (Lens::Band, theme::icon::WAVES, "Ta sama wys.", "Ta sama wysokość co pod kursorem (H)"),
        (Lens::Rel, theme::icon::PLUS_MINUS, "Względna", "Wysokość względem miejsca pod kursorem (W)"),
        (Lens::Vis, theme::icon::EYE, "Widoczność", "Co widać z miejsca pod kursorem (O)"),
    ];
    for (l, glyph, label, hint) in lenses {
        if theme::tool_tile(ui, glyph, label, a.lens == l, W).on_hover_text(format!("{hint}\nklik na mapie przypina, Esc odpina")).clicked() {
            a.set_lens(l);
        }
    }
    theme::bar_divider(ui);
    if theme::tool_tile(ui, theme::icon::MOUNTAINS, "Panorama", a.panorama.is_some(), W).on_hover_text("Panorama ze środka widoku (Esc – wyjście)").clicked() {
        if a.panorama.is_some() {
            a.exit_panorama();
        } else {
            let (lon, lat) = (a.cam.lon, a.cam.lat);
            a.enter_panorama(lon, lat);
        }
    }
}

/// Rozwijane menu spod przycisku paska.
fn bar_popup(ui: &egui::Ui, resp: &egui::Response, width: f32, body: impl FnOnce(&mut egui::Ui)) {
    egui::Popup::menu(resp)
        .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
        .frame(theme::glass().inner_margin(egui::Margin::same(14)))
        .show(|ui| {
            ui.set_width(width);
            let max_h = (ui.ctx().screen_rect().bottom() - ui.cursor().top() - 24.0).max(160.0);
            egui::ScrollArea::vertical().id_salt("menu-paska").max_height(max_h).show(ui, body);
        });
    let _ = ui;
}

fn popup_open(ui: &egui::Ui, resp: &egui::Response) -> bool {
    egui::Popup::is_id_open(ui.ctx(), egui::Popup::default_response_id(resp))
}

/// Pora dnia: godzina i ikona słońca; menu z datą, godziną i wschodem/zachodem.
fn sun_button(a: &mut Atlas, ui: &mut egui::Ui) {
    let (day, hour) = a.when();
    let custom = a.prefs.custom_time.is_some();
    let (_, alt) = a.sun();
    let glyph = if alt > 6.0 {
        theme::icon::SUN
    } else if alt > -6.0 {
        theme::icon::SUN_HORIZON
    } else {
        theme::icon::MOON_STARS
    };
    let sun_col = if custom {
        theme::BRASS
    } else if alt > 6.0 {
        theme::WARN
    } else if alt > -6.0 {
        Color32::from_rgb(255, 150, 90)
    } else {
        Color32::from_rgb(160, 180, 255)
    };
    let sub = if custom { day.label() } else { "teraz".to_string() };
    let id = egui::Id::new("pora-przycisk");
    let was_open = ui.ctx().data(|d| d.get_temp::<bool>(id)).unwrap_or(false);
    let resp = theme::bar_button(ui, glyph, sun_col, &fmt::hour(hour), &sub, 112.0, was_open)
        .on_hover_text(if custom { "Pora ustawiona ręcznie ([ ] – pół godziny)" } else { "Oświetlenie jak teraz ([ ] – pół godziny)" });
    bar_popup(ui, &resp, 340.0, |ui| {
        ui.label(RichText::new("Pora dnia i światło").size(15.0).strong());
        ui.add_space(4.0);
        light_tab(a, ui);
    });
    let open = popup_open(ui, &resp);
    ui.ctx().data_mut(|d| d.insert_temp(id, open));
}

pub fn style_glyph(st: Style) -> &'static str {
    match st {
        Style::Satellite => theme::icon::GLOBE_HEMISPHERE_EAST,
        Style::Terrain => theme::icon::MOUNTAINS,
        Style::Map => theme::icon::MAP_TRIFOLD,
        Style::Hypso => theme::icon::CHART_BAR,
        Style::Slope => theme::icon::ANGLE,
        Style::Aspect => theme::icon::COMPASS,
    }
}

/// Styl mapy: przycisk z bieżącym stylem, menu z kafelkami stylów.
fn style_button(a: &mut Atlas, ui: &mut egui::Ui) {
    let id = egui::Id::new("styl-przycisk");
    let was_open = ui.ctx().data(|d| d.get_temp::<bool>(id)).unwrap_or(false);
    let resp = theme::bar_button(ui, style_glyph(a.prefs.style), theme::BRASS, "Styl mapy", a.prefs.style.label(), 132.0, was_open).on_hover_text("Styl mapy (M – następny)");
    bar_popup(ui, &resp, 340.0, |ui| {
        ui.label(RichText::new("Styl mapy").size(15.0).strong());
        ui.add_space(4.0);
        crate::ui::layers_panel::style_tiles(a, ui);
    });
    let open = popup_open(ui, &resp);
    ui.ctx().data_mut(|d| d.insert_temp(id, open));
}

/// Widok: wbudowane składniki mapy – obiekty (nazwy, szlaki, poziomice, siatka) i teren
/// (ortofoto, cienie, rzeźba, pokrycie, śnieg). Pełny podział warstw – w panelu „Warstwy”.
fn view_button(a: &mut Atlas, ui: &mut egui::Ui) {
    use crate::layers::Kind;
    use crate::ui::atlas::BaseLayer;
    let n = BaseLayer::ALL.iter().filter(|b| b.on(&a.prefs)).count();
    let id = egui::Id::new("widok-przycisk");
    let was_open = ui.ctx().data(|d| d.get_temp::<bool>(id)).unwrap_or(false);
    let resp = theme::bar_button(ui, theme::icon::EYE, theme::BRASS, "Widok", &format!("włączone: {n}"), 118.0, was_open)
        .on_hover_text("Składniki mapy: nazwy, szlaki, poziomice, cienie, ortofoto");
    bar_popup(ui, &resp, 380.0, |ui| {
        ui.label(theme::kicker("Obiekty na mapie"));
        crate::ui::layers_panel::base_tiles(a, ui, Kind::Objects, 4);
        ui.add(egui::Slider::new(&mut a.prefs.label_density, 0.3..=2.0).text("gęstość nazw"));
        ui.add_space(8.0);
        ui.label(theme::kicker("Teren (warstwy globalne)"));
        crate::ui::layers_panel::base_tiles(a, ui, Kind::Global, 3);
        ui.add_space(6.0);
        if theme::row_button(ui, theme::icon::STACK, "Wszystkie warstwy… (G)", false).clicked() {
            a.prefs.panel_open = true;
            a.tab = Tab::Layers;
        }
    });
    let open = popup_open(ui, &resp);
    ui.ctx().data_mut(|d| d.insert_temp(id, open));
}

/// Panel warstw: zestawy, włączone warstwy (krycie, kolejność, legenda) i katalog z wyszukiwarką.
pub fn layers_tick(a: &mut Atlas, ctx: &egui::Context) {
    // legendy pobrane w tle
    let inbox: Vec<_> = std::mem::take(&mut *a.legend_inbox.lock().unwrap());
    for (i, img) in inbox {
        let tex = img.map(|m| {
            let size = [m.width() as usize, m.height() as usize];
            ctx.load_texture(format!("legenda-{i}"), egui::ColorImage::from_rgba_unmultiplied(size, m.as_raw()), egui::TextureOptions::LINEAR)
        });
        a.legends.insert(i, tex);
    }
}

/// Panel statystyki: wskaźnik GUS (temat → zmienna, albo wyszukiwarka), poziom, rok, klasy, legenda.
fn stats_body(a: &mut Atlas, ui: &mut egui::Ui) {
    use crate::stats::{self, Level, Method};
    let ctx = &ui.ctx().clone();
    {
        {
            ui.scope(|ui| {
                theme::para(ui, "Kartogram z Banku Danych Lokalnych GUS: wybierz wskaźnik z wyszukiwarki albo z drzewa tematów, potem poziom podziału i rok.");
                ui.add_space(4.0);
                let mut reload = false;
                ui.horizontal(|ui| {
                    for l in Level::ALL {
                        reload |= ui.selectable_value(&mut a.stats_level, l, l.label()).changed();
                    }
                });
                ui.horizontal(|ui| {
                    ui.label("Rok");
                    if ui.small_button("◀").clicked() {
                        a.stats_year -= 1;
                        reload = true;
                    }
                    ui.label(RichText::new(a.stats_year.to_string()).strong());
                    if ui.small_button("▶").clicked() {
                        a.stats_year += 1;
                        reload = true;
                    }
                    ui.separator();
                    let before = a.stats_method;
                    ui.selectable_value(&mut a.stats_method, Method::Quantiles, "kwantyle");
                    ui.selectable_value(&mut a.stats_method, Method::NaturalBreaks, "naturalne");
                    if before != a.stats_method {
                        stats::reclassify(a.stats_method);
                    }
                });
                ui.checkbox(&mut a.stats_bars, "Słupki 3D");
                if reload {
                    if let Some(v) = a.stats_var.clone() {
                        stats::load(&a.stats, v, a.stats_level, a.stats_year, a.stats_method, ctx);
                    }
                }
                if let Some(m) = stats::map() {
                    ui.add_space(6.0);
                    ui.label(RichText::new(&m.label).strong());
                    ui.label(RichText::new(format!("{} · {} · {} jednostek · {}", m.level.label(), m.year, m.values.len(), m.unit)).small().color(theme::PAPER_3));
                    let lo = m.values.values().fold(f64::MAX, |x, y| x.min(*y));
                    let hi = m.values.values().fold(f64::MIN, |x, y| x.max(*y));
                    for (i, c) in stats::PALETTE.iter().enumerate() {
                        let from = if i == 0 { lo } else { m.breaks[i - 1] };
                        let to = if i < stats::CLASSES - 1 { m.breaks[i] } else { hi };
                        ui.horizontal(|ui| {
                            let (r, _) = ui.allocate_exact_size(egui::vec2(22.0, 12.0), egui::Sense::hover());
                            ui.painter().rect_filled(r, 2.0, Color32::from_rgb(c[0], c[1], c[2]));
                            ui.label(RichText::new(format!("{} – {}", fmt::dec(from, 2), fmt::dec(to, 2))).small());
                        });
                    }
                    if ui.button("Usuń kartogram").clicked() {
                        stats::clear_map();
                        a.stats_var = None;
                    }
                }
                let (busy, progress, error) = {
                    let g = a.stats.lock().unwrap();
                    (g.busy, g.progress, g.error.clone())
                };
                if busy {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        let t = progress.map_or("pobieranie…".to_string(), |(x, y)| format!("pobieranie {x} z {y}"));
                        ui.label(RichText::new(t).small().color(theme::PAPER_3));
                    });
                }
                if let Some(e) = error {
                    ui.label(RichText::new(e).small().color(theme::WARN));
                }
                ui.separator();
                let mut pick = None;
                let r = ui.add(egui::TextEdit::singleline(&mut a.stats_query).hint_text("Szukaj wskaźnika, np. ludność, bezrobocie…").desired_width(f32::INFINITY));
                if r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) && !a.stats_query.trim().is_empty() {
                    stats::search(&a.stats, a.stats_query.trim(), ctx);
                }
                let found = a.stats.lock().unwrap().found.clone();
                for v in found {
                    variable_row(ui, &v, &mut pick);
                }
                ui.add_space(6.0);
                ui.label(theme::kicker("Tematy"));
                subject_tree(a, ui, "", &mut pick);
                if let Some(v) = pick {
                    // poziom ograniczony do tego, na którym wskaźnik istnieje
                    if v.level < 6 && a.stats_level == Level::Commune {
                        a.stats_level = Level::County;
                    }
                    if v.level < 5 {
                        a.stats_level = Level::Voivodeship;
                    }
                    a.stats_var = Some(v.clone());
                    stats::load(&a.stats, v, a.stats_level, a.stats_year, a.stats_method, ctx);
                }
            });
        }
    }
}

fn variable_row(ui: &mut egui::Ui, v: &crate::stats::Variable, pick: &mut Option<crate::stats::Variable>) {
    if ui.link(RichText::new(format!("{} [{}]", v.name, v.unit)).small()).clicked() {
        *pick = Some(v.clone());
    }
}

/// Drzewo tematów BDL, wczytywane przy rozwijaniu.
fn subject_tree(a: &mut Atlas, ui: &mut egui::Ui, parent: &str, pick: &mut Option<crate::stats::Variable>) {
    crate::stats::ask_subjects(&a.stats, parent, ui.ctx());
    let list = a.stats.lock().unwrap().subjects.get(parent).cloned();
    let Some(list) = list else {
        ui.spinner();
        return;
    };
    for s in list {
        egui::CollapsingHeader::new(RichText::new(&s.name).small()).id_salt(("temat", &s.id)).show(ui, |ui| {
            if s.has_vars {
                crate::stats::ask_variables(&a.stats, &s.id, ui.ctx());
                let vars = a.stats.lock().unwrap().variables.get(&s.id).cloned();
                match vars {
                    Some(vars) => {
                        for v in &vars {
                            variable_row(ui, v, pick);
                        }
                    }
                    None => {
                        ui.spinner();
                    }
                }
            } else {
                subject_tree(a, ui, &s.id, pick);
            }
        });
    }
}

/// Panel pogody: pole barwne, oś czasu, cząstki wiatru i satelita.
pub fn weather_tick(a: &mut Atlas, ctx: &egui::Context) {
    use crate::weather;
    // odtwarzanie działa także przy zwiniętym panelu
    let grid = weather::grid();
    if a.weather_field.is_some() || a.wind_particles {
        weather::ensure(ctx);
    }
    if let (true, Some(g)) = (a.weather_play, &grid) {
        if a.weather_tick() {
            let h = a.weather_hour.unwrap_or_else(|| g.now_index());
            a.weather_hour = Some((h + 1) % g.times.len());
        }
        ctx.request_repaint_after(std::time::Duration::from_millis(100));
    }
}

/// Zakładka „Pogoda”: pole barwne, oś czasu, cząstki wiatru i satelita.
fn weather_body(a: &mut Atlas, ui: &mut egui::Ui) {
    use crate::weather::{self, Field};
    let grid = weather::grid();
    {
        {
            ui.label(theme::kicker("Pole na mapie"));
            ui.horizontal_wrapped(|ui| {
                if ui.selectable_label(a.weather_field.is_none(), "Bez pola").clicked() {
                    a.weather_field = None;
                }
                for f in Field::ALL {
                    if ui.selectable_label(a.weather_field == Some(f), f.label()).clicked() {
                        a.weather_field = Some(f);
                    }
                }
            });
            ui.checkbox(&mut a.wind_particles, "Animacja wiatru");
            ui.checkbox(&mut a.satellite, "Obraz satelitarny IR (EUMETSAT, co 15 min)");
            if let Some(f) = a.weather_field {
                ui.add_space(4.0);
                ui.horizontal_wrapped(|ui| {
                    for (v, c) in f.ramp() {
                        let (r, _) = ui.allocate_exact_size(egui::vec2(16.0, 12.0), egui::Sense::hover());
                        ui.painter().rect_filled(r, 2.0, Color32::from_rgb(c[0], c[1], c[2]));
                        ui.label(RichText::new(format!("{v}")).small());
                    }
                    ui.label(RichText::new(f.unit()).small().color(theme::PAPER_3));
                });
            }
            ui.add_space(6.0);
            if weather::busy() {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(RichText::new("pobieranie prognozy…").small().color(theme::PAPER_3));
                });
            }
            if let Some(e) = weather::error() {
                ui.label(RichText::new(format!("serwis niedostępny: {e}")).small().color(theme::WARN));
            }
            match &grid {
                Some(g) => {
                    let mut h = a.weather_hour.unwrap_or_else(|| g.now_index());
                    ui.label(RichText::new(weather::time_label(g.times[h.min(g.times.len() - 1)])).size(18.0).strong());
                    ui.spacing_mut().slider_width = ui.available_width() - 12.0;
                    if ui.add(egui::Slider::new(&mut h, 0..=g.times.len() - 1).show_value(false)).changed() {
                        a.weather_hour = Some(h);
                    }
                    ui.horizontal(|ui| {
                        if ui.button(if a.weather_play { "Zatrzymaj" } else { "Odtwarzaj" }).clicked() {
                            a.weather_play = !a.weather_play;
                        }
                        if ui.button("Teraz").clicked() {
                            a.weather_hour = None;
                            a.weather_play = false;
                        }
                    });
                }
                None => {
                    theme::para(ui, "Wybierz pole albo animację wiatru, żeby pobrać prognozę.");
                }
            }
            ui.label(RichText::new("Siatka co 0,5° (około 50 km), dwa dni wstecz i trzy naprzód.").small().color(theme::PAPER_3));
        }
    }
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

/// Szerokość prawego panelu szczegółów.
const SIDEBAR_W: f32 = 460.0;

fn sidebar_open(a: &Atlas) -> bool {
    a.prefs.panel_open
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
                    if ui.small_button(theme::icon::CARET_LEFT).on_hover_text("Pokaż panel (P)").clicked() {
                        a.prefs.panel_open = true;
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
            // zakładki panelu: jedno miejsce na szczegóły, warstwy, statystykę i pogodę
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 2.0;
                // bieżąca zakładka z podpisem, pozostałe same ikony (podpis w podpowiedzi)
                let n = Tab::ALL.len() as f32;
                let small = 42.0;
                let wide = (ui.available_width() - 34.0 - (n - 1.0) * 2.0 - (n - 1.0) * small).max(small);
                for tab in Tab::ALL {
                    let on = a.tab == tab;
                    let (label, w) = if on { (tab.label(), wide) } else { ("", small) };
                    if theme::tab_button(ui, tab.icon(), label, on, w).on_hover_text(format!("{} ({})", tab.label(), tab.key())).clicked() {
                        a.tab = tab;
                    }
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.add(egui::Button::new(RichText::new(theme::icon::CARET_RIGHT).size(16.0)).frame(false).min_size(egui::vec2(30.0, 30.0))).on_hover_text("Ukryj panel (P)").clicked() {
                        a.prefs.panel_open = false;
                    }
                });
            });
            ui.add_space(4.0);
            ui.separator();
            egui::ScrollArea::vertical().id_salt(("panel", a.tab)).auto_shrink([false, false]).show(ui, |ui| match a.tab {
                Tab::Info => info(a, ui),
                Tab::Layers => crate::ui::layers_panel::body(a, ui),
                Tab::Library => crate::ui::library::body(a, ui),
                Tab::Stats => stats_body(a, ui),
                Tab::Weather => weather_body(a, ui),
            });
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
    ui.label(theme::kicker(&format!("Okolica · {}", crate::coords::format_pos(a.cam.lon, a.cam.lat))));
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
    if theme::row_button(ui, theme::icon::BOOKMARK_SIMPLE, "Zapisz widok z warstwami (Ctrl+D)", false).clicked() {
        a.save_view();
    }
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
    if theme::row_button(ui, theme::icon::STAR, "Dodaj do ulubionych", false).clicked() {
        let name = a.name_near(lon, lat, 600.0).unwrap_or_else(|| "Miejsce".to_string());
        a.add_place(&name, lon, lat);
    }
    if theme::row_button(ui, theme::icon::COPY, "Kopiuj współrzędne", false).clicked() {
        ui.ctx().copy_text(crate::coords::format_pos(lon, lat));
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

/// Współrzędne punktu we wszystkich układach używanych w Polsce, z kopiowaniem jednym kliknięciem.
fn coords_table(a: &mut Atlas, ui: &mut egui::Ui, lon: f64, lat: f64) {
    use crate::coords::{format_as, CoordFormat};
    let rows = [
        ("WGS 84", CoordFormat::Decimal),
        ("Stopnie", CoordFormat::Dms),
        ("PL-1992", CoordFormat::Pl1992),
        ("PL-2000", CoordFormat::Pl2000),
        ("UTM", CoordFormat::Utm),
    ];
    let mut copied = None;
    egui::Grid::new(("wspolrzedne", (lon * 1e5) as i64, (lat * 1e5) as i64)).num_columns(3).spacing([10.0, 3.0]).show(ui, |ui| {
        for (name, f) in rows {
            let text = format_as(f, lon, lat);
            let mine = crate::coords::format() == f;
            ui.label(RichText::new(name).small().color(if mine { theme::BRASS } else { theme::PAPER_3 }));
            ui.label(RichText::new(&text).monospace().size(12.5));
            if ui.small_button(theme::icon::COPY).on_hover_text("Kopiuj").clicked() {
                copied = Some(text);
            }
            ui.end_row();
        }
    });
    if let Some(t) = copied {
        ui.ctx().copy_text(t.clone());
        a.notify(format!("Skopiowano: {t}"));
    }
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
    ui.label(RichText::new(crate::coords::format_pos(p.poi.lon, p.poi.lat)).monospace().small().color(theme::PAPER_3));
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
    ]);
    coords_table(a, ui, lon, lat);
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
    if let (Some(f), Some(g)) = (a.weather_field, crate::weather::grid()) {
        let hour = a.weather_hour.unwrap_or_else(|| g.now_index());
        if let Some(v) = g.value(f, hour, lon, lat) {
            ui.add_space(6.0);
            ui.label(theme::kicker(&format!("{} · {}", f.label(), g.times.get(hour).map_or(String::new(), |t| crate::weather::time_label(*t)))));
            ui.label(RichText::new(format!("{} {}", fmt::dec(v as f64, 1), f.unit())).strong());
        }
    }
    if let Some((name, v, m)) = crate::stats::value_at(lon, lat) {
        ui.add_space(6.0);
        ui.label(theme::kicker(&format!("{} · {}", m.label, m.year)));
        ui.label(RichText::new(format!("{name}: {} {}", fmt::dec(v, 2), m.unit)).strong());
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
            ui.label(RichText::new(format!("{} · {}", crate::coords::format_pos(lon, lat), fmt::ele(e as f64))).monospace().small().color(theme::PAPER_3));
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
            theme::para(ui, format!("Poziom 0: {} · {}", fmt::ele(e as f64), crate::coords::format_pos(lon, lat)));
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

fn light_tab(a: &mut Atlas, ui: &mut egui::Ui) {
    let (day, hour) = a.when();
    let mut h = hour;
    let mut d = day;
    let t = sun::times(d, a.cam.lat, a.cam.lon);
    let (az, alt) = a.sun();
    ui.horizontal(|ui| {
        ui.label(RichText::new(fmt::hour(hour)).size(34.0).strong());
        ui.vertical(|ui| {
            ui.label(RichText::new(if a.prefs.custom_time.is_some() { "pora ustawiona" } else { "teraz" }).small().color(theme::PAPER_3));
            ui.label(RichText::new(format!("Słońce {}° · {} {}", fmt::dec(alt, 0), az.round(), fmt::compass(az))).small().color(theme::PAPER_2));
        });
    });
    // oś dnia: noc, świt, dzień, zmierzch i bieżąca godzina
    let (rect, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 14.0), egui::Sense::hover());
    let p = ui.painter();
    p.rect_filled(rect, 7.0, Color32::from_rgb(28, 34, 58));
    let x = |hh: f64| rect.left() + rect.width() * (hh / 24.0).clamp(0.0, 1.0) as f32;
    if t.set > t.rise {
        let day_r = egui::Rect::from_x_y_ranges(x(t.rise)..=x(t.set), rect.y_range());
        p.rect_filled(day_r, 7.0, Color32::from_rgb(232, 196, 96));
        for (a0, a1) in [(t.rise - 0.6, t.rise + 0.6), (t.set - 0.6, t.set + 0.6)] {
            p.rect_filled(egui::Rect::from_x_y_ranges(x(a0)..=x(a1), rect.y_range()), 4.0, Color32::from_rgb(240, 130, 80));
        }
    }
    p.circle(egui::pos2(x(hour), rect.center().y), 7.0, theme::PAPER, egui::Stroke::new(2.0, theme::INK_0));
    // suwak godziny i data na całą szerokość, data wyśrodkowana
    ui.spacing_mut().slider_width = ui.available_width();
    let r = ui.add(egui::Slider::new(&mut h, 0.0..=23.99).show_value(false));
    let (row, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 28.0), egui::Sense::hover());
    let bw = 30.0;
    let btn = |ui: &mut egui::Ui, x: f32, glyph: &str, hint: &str| {
        let rr = egui::Rect::from_min_size(egui::pos2(x, row.top()), egui::vec2(bw, row.height()));
        ui.put(rr, egui::Button::new(glyph).frame(false)).on_hover_text(hint).clicked()
    };
    if btn(ui, row.left(), theme::icon::CARET_DOUBLE_LEFT, "miesiąc wcześniej") {
        d = d.add_days(-30);
    }
    if btn(ui, row.left() + bw, theme::icon::CARET_LEFT, "dzień wcześniej") {
        d = d.add_days(-1);
    }
    if btn(ui, row.right() - 2.0 * bw, theme::icon::CARET_RIGHT, "dzień później") {
        d = d.add_days(1);
    }
    if btn(ui, row.right() - bw, theme::icon::CARET_DOUBLE_RIGHT, "miesiąc później") {
        d = d.add_days(30);
    }
    ui.painter().text(row.center(), egui::Align2::CENTER_CENTER, d.label(), egui::FontId::proportional(15.0), theme::PAPER);
    if r.changed() || d != day {
        a.prefs.custom_time = Some((d.y, d.m, d.d, h));
    }
    ui.add_space(6.0);
    let mut pick = None;
    theme::tile_grid(ui, 5, |ui, w| {
        if theme::tile(ui, theme::icon::CLOCK, "Teraz", a.prefs.custom_time.is_none(), w).clicked() {
            pick = Some(None);
        }
        if theme::tile(ui, theme::icon::SUN_HORIZON, "Wschód", false, w).clicked() {
            pick = Some(Some(t.rise + 0.25));
        }
        if theme::tile(ui, theme::icon::SUN, "Południe", false, w).clicked() {
            pick = Some(Some(t.noon));
        }
        if theme::tile(ui, theme::icon::SUN_DIM, "Złota godz.", false, w).clicked() {
            pick = Some(Some((t.set - 1.0).max(0.0)));
        }
        if theme::tile(ui, theme::icon::MOON_STARS, "Zachód", false, w).clicked() {
            pick = Some(Some(t.set));
        }
    });
    match pick {
        Some(None) => a.prefs.custom_time = None,
        Some(Some(hh)) => a.prefs.custom_time = Some((d.y, d.m, d.d, hh)),
        None => {}
    }
    ui.add_space(6.0);
    grid2(ui, vec![
        ("Wschód", fmt::hour(t.rise)),
        ("Zachód", fmt::hour(t.set)),
        ("Długość dnia", fmt::time((t.set - t.rise).max(0.0))),
        ("Górowanie", format!("{} · {}°", fmt::hour(t.noon), t.max_alt.round())),
    ]);
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

/// Okno skrótów: polecenia z rejestru (te same co w palecie Ctrl+K) i obsługa myszy.
pub fn help(a: &mut Atlas, ctx: &egui::Context) {
    use crate::ui::atlas::{Action, Group};
    if !a.help {
        return;
    }
    let mouse = [
        ("lewy przycisk", "przesuwanie (chwyć teren) · klik – szczegóły punktu"),
        ("prawy przycisk", "obrót i nachylenie · klik – menu analiz"),
        ("kółko", "przybliżanie do kursora"),
        ("dwuklik", "przybliż do punktu"),
        ("strzałki · Q E · PgUp PgDn", "przesuwanie · obrót · nachylenie"),
        ("klik z soczewką / Esc", "przypnij / odepnij soczewkę"),
        ("Esc", "anuluj / wyjdź"),
        ("?", "to okno"),
    ];
    let mut open = true;
    egui::Window::new("Skróty i sterowanie")
        .collapsible(false)
        .resizable(false)
        .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
        .open(&mut open)
        .show(ctx, |ui| {
            ui.set_width(720.0);
            theme::para(ui, "Wszystkie polecenia znajdziesz też w palecie (Ctrl+K) – wpisz kilka liter nazwy polecenia albo warstwy. Warstwy dzielą się na globalne (barwią całą mapę) i obiekty (rzeczy na mapie, zawsze na wierzchu).");
            ui.add_space(6.0);
            egui::ScrollArea::vertical().max_height(ctx.screen_rect().height() * 0.7).show(ui, |ui| {
                ui.columns(2, |cols| {
                    for (n, g) in Group::ALL.iter().enumerate() {
                        let ui = &mut cols[if n < 3 { 0 } else { 1 }];
                        let acts: Vec<Action> = Action::all().into_iter().filter(|x| x.info().2 == *g && x.keys().is_some()).collect();
                        if acts.is_empty() {
                            continue;
                        }
                        ui.label(theme::kicker(g.label()));
                        egui::Grid::new(("skroty", n)).num_columns(2).striped(true).spacing([14.0, 4.0]).show(ui, |ui| {
                            for x in acts {
                                ui.label(x.info().1);
                                ui.label(RichText::new(x.keys().map(|k| k.text()).unwrap_or_default()).monospace().color(theme::BRASS));
                                ui.end_row();
                            }
                        });
                        ui.add_space(8.0);
                    }
                    let ui = &mut cols[1];
                    ui.label(theme::kicker("Mysz i klawiatura"));
                    egui::Grid::new("skroty-mysz").num_columns(2).striped(true).spacing([14.0, 4.0]).show(ui, |ui| {
                        for (k, v) in mouse {
                            ui.label(v);
                            ui.label(RichText::new(k).monospace().color(theme::BRASS));
                            ui.end_row();
                        }
                    });
                });
            });
        });
    if !open {
        a.help = false;
    }
}

/// Strona „Mapa” w ustawieniach: zapis współrzędnych, legenda i porządek warstw.
pub fn map_settings(a: &mut Atlas, ui: &mut egui::Ui) {
    ui.label(theme::kicker("Zapis współrzędnych"));
    for f in crate::coords::CoordFormat::ALL {
        if theme::option_card(ui, f.name(), &f.example(), a.prefs.coord_format == f).clicked() {
            a.prefs.coord_format = f;
            crate::coords::set_format(f);
        }
    }
    theme::para(ui, "Tak współrzędne pokazują się przy kursorze, w szczegółach punktu i przy kopiowaniu. PL-1992 to układ map urzędowych (Geoportal, BDOT10k), PL-2000 – mapy zasadniczej i ewidencji gruntów. W wyszukiwarkę można wpisać współrzędne w każdym z tych zapisów, także X i Y w metrach.");
    ui.add_space(12.0);
    ui.label(theme::kicker("Warstwy"));
    theme::switch(ui, &mut a.prefs.legend, "Legenda na mapie (J)");
    theme::para(ui, "Warstwy globalne barwią całą mapę i zawsze leżą pod spodem: podkład, rzeźba, pokrycie terenu, geologia, plany, pogoda i kartogramy GUS. Obiekty – nazwy, szlaki, granice, działki, budynki, sieci, punkty i dane na żywo – są rysowane na wierzchu. Kolejność w każdej grupie ustawisz strzałkami w panelu warstw.");
}
