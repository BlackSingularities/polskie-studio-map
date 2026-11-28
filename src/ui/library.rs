//! Zakładka „Biblioteka” (jak w GRANI): zapisane widoki z warstwami i ulubione miejsca – otwieranie,
//! zmiana nazwy, wyszukiwanie i usuwanie.

use eframe::egui::{self, RichText};

use crate::ui::atlas::{Atlas, Selection};
use crate::ui::panels::Tab;
use crate::ui::search::fold;
use crate::ui::theme;

enum Act {
    OpenView(u64),
    OpenPlace(u64),
    Rename(u64, String),
    StartRename(u64, String),
    CancelRename,
    Remove(u64),
}

pub fn body(a: &mut Atlas, ui: &mut egui::Ui) {
    theme::tile_grid(ui, 2, |ui, w| {
        if theme::tile(ui, theme::icon::BOOKMARK_SIMPLE, "Zapisz widok (Ctrl+D)", false, w).on_hover_text("Kamera, styl mapy i wszystkie włączone warstwy").clicked() {
            a.save_view();
        }
        if theme::tile(ui, theme::icon::STAR, "Środek widoku do ulubionych", false, w).clicked() {
            let (lon, lat) = (a.cam.lon, a.cam.lat);
            let name = a.name_near(lon, lat, 1500.0).unwrap_or_else(|| "Miejsce".to_string());
            a.add_place(&name, lon, lat);
        }
    });
    ui.add_space(6.0);
    let total = a.library.views.len() + a.library.places.len();
    if total == 0 {
        theme::para(ui, "Biblioteka jest pusta. Zapisany widok pamięta położenie kamery, styl mapy i cały stos warstw – globalnych i obiektów – z kryciem i kolejnością. Miejsca dodasz też z menu pod prawym przyciskiem myszy albo z panelu szczegółów punktu.");
        return;
    }
    if total > 6 {
        ui.add(egui::TextEdit::singleline(&mut a.lib_query).hint_text("Szukaj w bibliotece…").desired_width(f32::INFINITY));
    }
    let q = fold(a.lib_query.trim());
    let mut act = None;

    let views: Vec<_> = a.library.views.iter().filter(|v| q.is_empty() || fold(&v.name).contains(&q)).cloned().collect();
    theme::section(ui, theme::icon::BOOKMARKS, "Widoki", &views.len().to_string());
    if views.is_empty() {
        theme::para(ui, "Brak zapisanych widoków.");
    }
    for v in &views {
        item(ui, a.lib_edit.as_ref(), v.id, theme::icon::MAP_TRIFOLD, &v.name, &v.summary(), &mut act, true);
    }

    let places: Vec<_> = a.library.places.iter().filter(|p| q.is_empty() || fold(&p.name).contains(&q)).cloned().collect();
    theme::section(ui, theme::icon::STAR, "Ulubione miejsca", &places.len().to_string());
    if places.is_empty() {
        theme::para(ui, "Brak ulubionych miejsc.");
    }
    for p in &places {
        let sub = format!("{} · {}", crate::coords::format_pos(p.lon, p.lat), crate::library::date_label(p.created));
        item(ui, a.lib_edit.as_ref(), p.id, theme::icon::MAP_PIN, &p.name, &sub, &mut act, false);
    }

    match act {
        Some(Act::OpenView(id)) => {
            if let Some(v) = a.library.views.iter().find(|v| v.id == id).cloned() {
                a.apply_view(&v);
                a.notify(format!("Otwarto widok „{}”.", v.name));
            }
        }
        Some(Act::OpenPlace(id)) => {
            if let Some(p) = a.library.places.iter().find(|p| p.id == id).cloned() {
                a.fly_to(p.lon, p.lat, Some(a.cam.distance.clamp(1500.0, 6000.0)));
                a.selection = Selection::Point { lon: p.lon, lat: p.lat };
                a.tab = Tab::Info;
            }
        }
        Some(Act::StartRename(id, name)) => a.lib_edit = Some((id, name)),
        Some(Act::CancelRename) => a.lib_edit = None,
        Some(Act::Rename(id, name)) => {
            a.library.rename(id, &name);
            a.lib_edit = None;
            a.store_library();
        }
        Some(Act::Remove(id)) => {
            a.library.remove(id);
            a.store_library();
        }
        None => {}
    }
}

/// Wiersz biblioteki: ikona, nazwa (albo pole zmiany nazwy), opis i przyciski.
#[allow(clippy::too_many_arguments)]
fn item(ui: &mut egui::Ui, edit: Option<&(u64, String)>, id: u64, glyph: &str, name: &str, sub: &str, act: &mut Option<Act>, view: bool) {
    let editing = edit.filter(|e| e.0 == id).map(|e| e.1.clone());
    theme::card(false).inner_margin(egui::Margin::symmetric(10, 8)).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.horizontal(|ui| {
            ui.label(RichText::new(glyph).size(18.0).color(theme::BRASS));
            if let Some(mut text) = editing {
                let r = ui.add(egui::TextEdit::singleline(&mut text).desired_width(ui.available_width() - 70.0));
                r.request_focus();
                let (enter, esc) = ui.input(|i| (i.key_pressed(egui::Key::Enter), i.key_pressed(egui::Key::Escape)));
                if enter || ui.small_button(theme::icon::CHECK).on_hover_text("Zapisz nazwę").clicked() {
                    *act = Some(Act::Rename(id, text));
                } else if esc || ui.small_button(theme::icon::X).on_hover_text("Anuluj").clicked() {
                    *act = Some(Act::CancelRename);
                } else if r.changed() {
                    *act = Some(Act::StartRename(id, text));
                }
                return;
            }
            let title = ui.add(egui::Label::new(RichText::new(name).strong()).truncate().sense(egui::Sense::click()));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.small_button(theme::icon::TRASH).on_hover_text("Usuń z biblioteki").clicked() {
                    *act = Some(Act::Remove(id));
                }
                if ui.small_button(theme::icon::PENCIL_SIMPLE).on_hover_text("Zmień nazwę").clicked() {
                    *act = Some(Act::StartRename(id, name.to_string()));
                }
                let open = if view { "Otwórz" } else { "Pokaż" };
                if ui.small_button(open).clicked() {
                    *act = Some(if view { Act::OpenView(id) } else { Act::OpenPlace(id) });
                }
            });
            if title.clicked() {
                *act = Some(if view { Act::OpenView(id) } else { Act::OpenPlace(id) });
            }
        });
        ui.label(RichText::new(sub).small().color(theme::PAPER_3));
    });
    ui.add_space(2.0);
}
