//! Paleta poleceń (Ctrl+K, jak w GRANI): wszystkie polecenia programu z rejestru [`Action`]
//! z wyszukiwaniem, obsługą strzałkami i Enterem. Do tego warstwy z katalogu (włączanie
//! i wyłączanie po nazwie) i zapisane widoki. Gdy nic nie pasuje, tekst można wyszukać na mapie.

use eframe::egui::{self, RichText};

use crate::layers::{self, Kind};
use crate::ui::atlas::{Action, Atlas};
use crate::ui::search::fold;
use crate::ui::theme;

#[derive(Default)]
pub struct Palette {
    pub q: String,
    sel: usize,
    focused: bool,
}

/// Wiersz palety.
#[derive(Clone, Copy, PartialEq)]
enum Item {
    Act(Action),
    /// Warstwa z katalogu (indeks).
    Layer(u16),
    /// Zapisany widok (identyfikator).
    View(u64),
    SearchMap,
}

/// Polecenia pasujące do zapytania: każde słowo musi wystąpić w nazwie, grupie albo skrócie.
fn matches(a: &Atlas, q: &str) -> Vec<Item> {
    let words: Vec<String> = fold(q).split_whitespace().map(str::to_string).collect();
    let fits = |hay: &str| {
        let hay = fold(hay);
        words.iter().all(|w| hay.contains(w.as_str()))
    };
    let mut v: Vec<Item> = Action::all()
        .into_iter()
        .filter(|act| {
            let (_, label, group) = act.info();
            let keys = act.keys().map(|k| k.text()).unwrap_or_default();
            fits(&format!("{label} {} {keys}", group.label()))
        })
        .map(Item::Act)
        .collect();
    // warstwy i widoki tylko przy wpisanym tekście – inaczej lista byłaby za długa
    if !words.is_empty() {
        for (i, d) in layers::catalog().layers.iter().enumerate() {
            if fits(&format!("{} {} warstwa {}", d.name, d.group, d.kind.title())) {
                v.push(Item::Layer(i as u16));
            }
        }
        for view in &a.library.views {
            if fits(&format!("{} widok biblioteka", view.name)) {
                v.push(Item::View(view.id));
            }
        }
    }
    v
}

pub fn show(a: &mut Atlas, ctx: &egui::Context) {
    let Some(mut p) = a.palette.take() else { return };
    let mut items = matches(a, &p.q);
    if !p.q.trim().is_empty() {
        items.push(Item::SearchMap);
    }
    p.sel = p.sel.min(items.len().saturating_sub(1));
    let (up, down, enter, esc) = ctx.input(|i| {
        (
            i.key_pressed(egui::Key::ArrowUp),
            i.key_pressed(egui::Key::ArrowDown),
            i.key_pressed(egui::Key::Enter),
            i.key_pressed(egui::Key::Escape),
        )
    });
    if up {
        p.sel = p.sel.checked_sub(1).unwrap_or(items.len().saturating_sub(1));
    }
    if down && !items.is_empty() {
        p.sel = (p.sel + 1) % items.len();
    }
    let mut run: Option<Item> = enter.then(|| items.get(p.sel).copied()).flatten();
    let mut close = esc;

    let w = (ctx.screen_rect().width() * 0.5).clamp(380.0, 640.0);
    let resp = egui::Area::new(egui::Id::new("paleta-polecen"))
        .order(egui::Order::Foreground)
        .anchor(egui::Align2::CENTER_TOP, egui::vec2(0.0, theme::TOP_BAR_H + 40.0))
        .show(ctx, |ui| {
            theme::glass().inner_margin(egui::Margin::same(12)).show(ui, |ui| {
                ui.set_width(w);
                ui.horizontal(|ui| {
                    ui.label(RichText::new(theme::icon::COMMAND).size(18.0).color(theme::BRASS));
                    let te = egui::TextEdit::singleline(&mut p.q)
                        .hint_text("Wpisz polecenie albo warstwę, np. „działki”, „legenda”, „hipsometria”…")
                        .desired_width(f32::INFINITY)
                        .frame(false)
                        .font(egui::FontId::proportional(16.0));
                    let r = ui.add(te);
                    if !p.focused {
                        r.request_focus();
                        p.focused = true;
                    }
                    if r.changed() {
                        p.sel = 0;
                    }
                });
                ui.separator();
                egui::ScrollArea::vertical().max_height(440.0).auto_shrink([false, true]).show(ui, |ui| {
                    if items.is_empty() {
                        ui.label(RichText::new("Brak pasujących poleceń.").color(theme::PAPER_3));
                    }
                    let mut last_group: Option<String> = None;
                    for (n, item) in items.iter().enumerate() {
                        let selected = n == p.sel;
                        let (glyph, label, group, keys, state, enabled) = describe(a, *item, &p.q);
                        if p.q.trim().is_empty() && group != last_group {
                            if let Some(g) = &group {
                                ui.add_space(4.0);
                                ui.label(theme::kicker(g));
                            }
                            last_group = group.clone();
                        }
                        let (rect, r) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 32.0), egui::Sense::click());
                        if selected {
                            r.scroll_to_me(None);
                        }
                        let painter = ui.painter();
                        if selected || r.hovered() {
                            painter.rect_filled(rect, 8.0, if selected { theme::BRASS_DIM } else { egui::Color32::from_white_alpha(14) });
                        }
                        let color = if !enabled {
                            theme::INK_4
                        } else if selected {
                            theme::BRASS
                        } else {
                            theme::PAPER
                        };
                        painter.text(rect.left_center() + egui::vec2(10.0, 0.0), egui::Align2::LEFT_CENTER, glyph, egui::FontId::proportional(17.0), color);
                        let mut x = rect.right() - 10.0;
                        if let Some(k) = keys {
                            let g = painter.layout_no_wrap(k, egui::FontId::monospace(11.5), theme::PAPER_2);
                            let kr = egui::Rect::from_min_size(egui::pos2(x - g.size().x - 10.0, rect.center().y - g.size().y / 2.0 - 3.0), g.size() + egui::vec2(10.0, 6.0));
                            painter.rect_stroke(kr, 5.0, egui::Stroke::new(1.0, theme::INK_4), egui::StrokeKind::Inside);
                            painter.galley(kr.min + egui::vec2(5.0, 3.0), g, theme::PAPER_2);
                            x = kr.left() - 10.0;
                        }
                        if let Some(on) = state {
                            let t = if on { "wł." } else { "wył." };
                            let g = painter.layout_no_wrap(t.to_string(), egui::FontId::proportional(12.0), if on { theme::OK } else { theme::PAPER_3 });
                            painter.galley(egui::pos2(x - g.size().x, rect.center().y - g.size().y / 2.0), g, theme::PAPER_3);
                            x -= 40.0;
                        }
                        let clip = egui::Rect::from_min_max(rect.left_top(), egui::pos2(x, rect.bottom()));
                        painter.with_clip_rect(clip).text(
                            rect.left_center() + egui::vec2(38.0, 0.0),
                            egui::Align2::LEFT_CENTER,
                            &label,
                            egui::FontId::proportional(14.0),
                            if enabled { theme::PAPER } else { theme::PAPER_3 },
                        );
                        if r.clicked() {
                            run = Some(*item);
                        }
                        if r.hovered() && ui.input(|i| i.pointer.delta() != egui::Vec2::ZERO) {
                            p.sel = n;
                        }
                    }
                });
                ui.add_space(4.0);
                ui.label(RichText::new("↑ ↓ wybór · Enter wykonuje · Esc zamyka").small().color(theme::PAPER_3));
            });
        });
    // klik poza paletą zamyka ją
    if resp.response.clicked_elsewhere() {
        close = true;
    }
    match run {
        Some(Item::Act(act)) if act.enabled(a) => {
            a.palette = None;
            a.run(act, ctx);
            return;
        }
        Some(Item::Layer(i)) => {
            let id = layers::catalog().layers[i as usize].id.clone();
            let kind = layers::kind_of(i);
            if let Some(l) = a.prefs.layers.iter_mut().find(|l| l.id == id) {
                l.on = !l.on;
                let msg = format!("{}: {}", if l.on { "Włączono" } else { "Wyłączono" }, layers::catalog().layers[i as usize].name);
                a.prefs.layer_kind = kind;
                a.notify(msg);
            }
            close = true;
        }
        Some(Item::View(id)) => {
            if let Some(v) = a.library.views.iter().find(|v| v.id == id).cloned() {
                a.apply_view(&v);
            }
            close = true;
        }
        Some(Item::SearchMap) => {
            close = true;
            a.search = Some(crate::ui::search::Search::with_query(p.q.trim()));
        }
        _ => {}
    }
    if !close {
        a.palette = Some(p);
    }
}

/// Ikona, podpis, grupa, skrót, stan przełącznika i dostępność wiersza.
fn describe(a: &Atlas, item: Item, q: &str) -> (&'static str, String, Option<String>, Option<String>, Option<bool>, bool) {
    match item {
        Item::Act(act) => {
            let (g, l, gr) = act.info();
            (g, l, Some(gr.label().to_string()), act.keys().map(|k| k.text()), act.checked(a), act.enabled(a))
        }
        Item::Layer(i) => {
            let d = &layers::catalog().layers[i as usize];
            let on = a.prefs.layers.iter().any(|l| l.id == d.id && l.on);
            let glyph = match d.kind {
                Kind::Global => theme::icon::GLOBE_HEMISPHERE_EAST,
                Kind::Objects => theme::icon::MAP_PIN,
            };
            (glyph, format!("{} · {}", d.name, d.group), Some("Warstwy z katalogu".into()), None, Some(on), true)
        }
        Item::View(id) => {
            let name = a.library.views.iter().find(|v| v.id == id).map_or(String::new(), |v| v.name.clone());
            (theme::icon::BOOKMARK_SIMPLE, format!("Otwórz widok „{name}”"), Some("Biblioteka".into()), None, None, true)
        }
        Item::SearchMap => (theme::icon::MAGNIFYING_GLASS, format!("Szukaj „{}” na mapie", q.trim()), None, Some("/".to_string()), None, true),
    }
}
