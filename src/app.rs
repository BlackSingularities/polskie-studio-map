//! Główna aplikacja: mapa 3D na całe okno i unoszący się nad nią interfejs. Dane zapisują się same
//! w miarę oglądania, a sprzątacz pilnuje limitu rozmiaru.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use eframe::egui::{self, RichText};

use crate::heights::Heights;
use crate::settings::{data_dir_in, Settings, LIMIT_STEPS_GB};
use crate::store::{fmt_bytes, free_space, Store};
use crate::ui::atlas::Atlas;
use crate::ui::theme;
use crate::updater;
use crate::world::World;

/// Stan aktualizacji programu.
enum Update {
    Idle,
    Checking(Arc<Mutex<Option<Result<updater::Release, String>>>>, bool),
    Available(updater::Release),
    Downloading(updater::Release, Arc<AtomicU64>, Arc<Mutex<Option<Result<PathBuf, String>>>>),
}

/// Zajętość dysku liczona w tle (przejście po tysiącach plików nie może blokować interfejsu).
struct Usage {
    bytes: AtomicU64,
    known: AtomicBool,
    busy: AtomicBool,
}

/// Przeniesienie folderu danych w tle.
struct Moving {
    to: PathBuf,
    result: Arc<Mutex<Option<Result<(), String>>>>,
    settings: Settings,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Page {
    General,
    Graphics,
    Data,
    About,
}

const PAGES: [(Page, &str, &str); 4] = [
    (Page::General, theme::icon::SLIDERS_HORIZONTAL, "Ogólne"),
    (Page::Graphics, theme::icon::MONITOR, "Grafika"),
    (Page::Data, theme::icon::HARD_DRIVE, "Dane map"),
    (Page::About, theme::icon::INFO, "O programie"),
];

/// Otwarte ustawienia: kopia robocza i bieżąca strona.
struct SettingsView {
    draft: Settings,
    page: Page,
}

pub struct App {
    settings: Settings,
    store: Store,
    usage: Arc<Usage>,
    last_gc: Instant,
    settings_open: Option<SettingsView>,
    moving: Option<Moving>,
    /// Zrzut okna do pliku (PSM_CAPTURE=plik.png, PSM_CAPTURE_AFTER=sekundy) – zrzuty do dokumentacji i testy.
    capture: Option<(PathBuf, Instant, f32, bool)>,
    heights: Heights,
    world: World,
    atlas: Atlas,
    update: Update,
}

/// Klucz ustawień widoku; nowy układ zaczyna od nowych domyślnych.
const PREFS_KEY: &str = "atlas2";

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        theme::apply(&cc.egui_ctx);
        let settings = Settings::load();
        let store = Store::new(&settings.data_dir);
        let heights = Heights::new(store.clone());
        let world = World::new(heights.clone(), cc.egui_ctx.clone());
        let prefs = cc.storage.and_then(|s| eframe::get_value::<crate::ui::atlas::Prefs>(s, PREFS_KEY));
        let atlas = Atlas::new(heights.clone(), world.clone(), prefs);
        updater::cleanup();
        let mut app = Self {
            heights,
            world,
            atlas,
            update: Update::Idle,
            settings,
            store,
            usage: Arc::new(Usage {
                bytes: AtomicU64::new(0),
                known: AtomicBool::new(false),
                busy: AtomicBool::new(false),
            }),
            last_gc: Instant::now(),
            settings_open: None,
            moving: None,
            capture: std::env::var_os("PSM_CAPTURE").map(|p| {
                (
                    PathBuf::from(p),
                    Instant::now(),
                    std::env::var("PSM_CAPTURE_AFTER")
                        .ok()
                        .and_then(|s| s.parse().ok())
                        .unwrap_or(6.0),
                    false,
                )
            }),
        };
        app.collect_garbage();
        // link psm:// z wiersza poleceń (kliknięty w systemie)
        if let Some(l) = std::env::args().find(|a| a.starts_with("psm://")) {
            app.atlas.apply_link(&l);
        }
        if app.settings.auto_update && std::env::var_os("PSM_CAPTURE").is_none() {
            app.check_updates(false);
        }
        app
    }

    // ------------------------------------------------------------------ dane

    /// Sprzątanie do limitu (najdawniej używane kafle) i przeliczenie zajętości, w tle.
    fn collect_garbage(&mut self) {
        self.last_gc = Instant::now();
        if self.usage.busy.swap(true, Ordering::Relaxed) {
            return;
        }
        let (store, limit, usage) = (self.store.clone(), self.settings.limit_bytes(), self.usage.clone());
        thread::spawn(move || {
            let (n, freed) = store.collect_garbage(limit);
            if n > 0 {
                log::info!("sprzątanie: usunięto {n} paczek ({})", fmt_bytes(freed));
            }
            usage.bytes.store(store.usage(), Ordering::Relaxed);
            usage.known.store(true, Ordering::Relaxed);
            usage.busy.store(false, Ordering::Relaxed);
        });
    }

    // ------------------------------------------------------------------ ustawienia i przenoszenie

    fn open_settings(&mut self, page: Page) {
        self.settings_open = Some(SettingsView { draft: self.settings.clone(), page });
    }

    fn apply_settings(&mut self, next: Settings) {
        let moved = next.data_dir != self.settings.data_dir;
        if moved
            && Path::new(&self.settings.data_dir).exists()
            && self.usage.bytes.load(Ordering::Relaxed) > 0
        {
            let r = rfd::MessageDialog::new()
                .set_title("Polskie Studio Map")
                .set_description(format!(
                    "Przenieść zapisane dane do nowego folderu?\n\nZ: {}\nDo: {}\n\nTak – dane trafią do nowego folderu, a stary zostanie usunięty.\nNie – nowy folder zacznie się wypełniać od początku, stare dane zostaną w poprzednim miejscu.",
                    self.settings.data_dir.display(),
                    next.data_dir.display()
                ))
                .set_buttons(rfd::MessageButtons::YesNoCancel)
                .show();
            match r {
                rfd::MessageDialogResult::Yes => return self.start_move(next),
                rfd::MessageDialogResult::No => {}
                _ => return,
            }
        }
        self.commit_settings(next);
    }

    fn commit_settings(&mut self, next: Settings) {
        let moved = next.data_dir != self.settings.data_dir;
        self.settings = next;
        if let Err(e) = self.settings.save() {
            self.atlas.notify(format!("Nie udało się zapisać ustawień: {e}"));
        }
        if moved {
            self.store = Store::new(&self.settings.data_dir);
            self.heights.set_store(self.store.clone());
            self.world.reset();
            if let Some(r) = &mut self.atlas.renderer {
                r.tiles.reset();
            }
        }
        self.collect_garbage();
    }

    fn start_move(&mut self, next: Settings) {
        let (from, to) = (self.settings.data_dir.clone(), next.data_dir.clone());
        let result = Arc::new(Mutex::new(None));
        let r = result.clone();
        thread::spawn(move || {
            *r.lock().unwrap() = Some(move_dir(&from, &to).map_err(|e| e.to_string()));
        });
        self.moving = Some(Moving {
            to: next.data_dir.clone(),
            result,
            settings: next,
        });
    }

    // ------------------------------------------------------------------ widoki

    /// Prawy górny róg: aktualizacja, pomoc, ustawienia.
    fn top_right(&mut self, ctx: &egui::Context) {
        let mut offer = None;
        let mut settings = false;
        egui::Area::new(egui::Id::new("gora-prawa"))
            .anchor(egui::Align2::RIGHT_TOP, [-16.0 - crate::ui::panels::right_inset(&self.atlas), 16.0])
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    if let Update::Available(rel) = &self.update {
                        if ui.add(theme::primary(&format!("Aktualizacja {}", rel.version))).clicked() {
                            offer = Some(rel.clone());
                        }
                    }
                    theme::glass().show(ui, |ui| {
                        ui.horizontal(|ui| {
                            crate::ui::panels::view_menus(&mut self.atlas, ui);
                            ui.separator();
                            if theme::icon_button(ui, theme::icon::QUESTION, self.atlas.help).on_hover_text("Skróty i sterowanie (?)").clicked() {
                                self.atlas.help = !self.atlas.help;
                            }
                            if theme::icon_button(ui, theme::icon::GEAR, false).on_hover_text("Ustawienia (Ctrl+,)").clicked() {
                                settings = true;
                            }
                        });
                    });
                });
            });
        if settings {
            self.open_settings(Page::General);
        }
        if let Some(rel) = offer {
            self.offer_update(rel);
        }
    }

    fn settings_window(&mut self, ctx: &egui::Context) {
        let Some(view) = &mut self.settings_open else {
            return;
        };
        let mut close = false;
        let mut save = false;
        let mut check = false;
        let used = self.usage.bytes.load(Ordering::Relaxed);
        let known = self.usage.known.load(Ordering::Relaxed);
        let current_limit = self.settings.limit_gb;
        let atlas = &mut self.atlas;
        let modal = egui::Modal::new(egui::Id::new("ustawienia")).frame(theme::glass().inner_margin(egui::Margin::same(18))).show(ctx, |ui| {
            ui.set_width(760.0);
            ui.horizontal(|ui| {
                ui.heading("Ustawienia");
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.small_button(theme::icon::X).on_hover_text("Zamknij bez zapisywania (Esc)").clicked() {
                        close = true;
                    }
                });
            });
            ui.add_space(8.0);
            ui.horizontal_top(|ui| {
                ui.allocate_ui_with_layout(egui::vec2(180.0, 440.0), egui::Layout::top_down(egui::Align::Min), |ui| {
                    ui.set_width(180.0);
                    for (page, glyph, name) in PAGES {
                        if theme::row_button(ui, glyph, name, view.page == page).clicked() {
                            view.page = page;
                        }
                    }
                });
                ui.add_space(12.0);
                ui.allocate_ui_with_layout(egui::vec2(ui.available_width(), 440.0), egui::Layout::top_down(egui::Align::Min), |ui| {
                    ui.set_height(440.0);
                    egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| match view.page {
                        Page::General => {
                            ui.label(theme::kicker("Program"));
                            theme::switch(ui, &mut view.draft.auto_update, "Sprawdzaj aktualizacje");
                            theme::switch(ui, &mut view.draft.vsync, "Synchronizacja z monitorem");
                            theme::para(ui, "Bez synchronizacji więcej klatek na sekundę, ale możliwe rozrywanie obrazu. Zmiana działa po ponownym uruchomieniu.");
                        }
                        Page::Graphics => crate::ui::panels::graphics(atlas, ui),
                        Page::Data => {
                            ui.label(theme::kicker("Zajęte miejsce"));
                            ui.horizontal(|ui| {
                                ui.label(RichText::new(if known { fmt_bytes(used) } else { "…".into() }).size(22.0).strong());
                                ui.label(RichText::new(format!("z {current_limit} GB")).color(theme::PAPER_3));
                            });
                            let limit = current_limit as u64 * (1 << 30);
                            theme::bar(ui, used as f32 / limit as f32, theme::BRASS);
                            ui.add_space(12.0);
                            ui.label(theme::kicker("Limit danych"));
                            let mut idx = LIMIT_STEPS_GB.iter().position(|&g| g >= view.draft.limit_gb).unwrap_or(LIMIT_STEPS_GB.len() - 1);
                            ui.horizontal(|ui| {
                                if ui.add(egui::Slider::new(&mut idx, 0..=LIMIT_STEPS_GB.len() - 1).show_value(false)).changed() {
                                    view.draft.limit_gb = LIMIT_STEPS_GB[idx];
                                }
                                ui.label(RichText::new(format!("{} GB", view.draft.limit_gb)).strong());
                            });
                            theme::para(ui, "Mapy zapisują się same podczas oglądania. Po przekroczeniu limitu najdawniej używane dane są usuwane.");
                            ui.add_space(12.0);
                            ui.label(theme::kicker("Folder danych"));
                            ui.label(RichText::new(view.draft.data_dir.display().to_string()).monospace());
                            if let Some(f) = free_space(&view.draft.data_dir) {
                                ui.label(RichText::new(format!("Wolne miejsce: {}", fmt_bytes(f))).small().color(theme::PAPER_3));
                            }
                            if ui.button(theme::ic(theme::icon::FOLDER_OPEN, "Zmień folder…")).clicked() {
                                if let Some(p) = rfd::FileDialog::new().set_title("Folder na dane map").pick_folder() {
                                    view.draft.data_dir = data_dir_in(&p);
                                }
                            }
                        }
                        Page::About => {
                            ui.label(RichText::new(format!("Polskie Studio Map {}", env!("CARGO_PKG_VERSION"))).size(22.0).strong());
                            if ui.button(theme::ic(theme::icon::ARROWS_CLOCKWISE, "Sprawdź aktualizacje")).clicked() {
                                check = true;
                            }
                            ui.add_space(12.0);
                            ui.label(theme::kicker("Autor"));
                            if ui.link("redzikowski.com").clicked() {
                                let _ = open::that("https://redzikowski.com");
                            }
                            ui.label(RichText::new("kontakt@redzikowski.com").small().color(theme::PAPER_3));
                            if ui.add(theme::primary(&theme::ic(theme::icon::COFFEE, "Postaw kawę"))).clicked() {
                                let _ = open::that("https://buycoffee.to/adamredzikowski");
                            }
                            ui.add_space(12.0);
                            ui.label(theme::kicker("Źródła danych"));
                            theme::para(ui, "Teren: NMT i NMPT GUGiK, Mapterhorn z NMT GUGiK (CC BY 4.0), zapasowo Terrarium/AWS Open Data. Zdjęcia: Sentinel-2 cloudless 2020 © EOX (CC BY-NC-SA 4.0), ortofoto: GUGiK. Nazwy: OpenFreeMap, OpenMapTiles. Szlaki i punkty: © OpenStreetMap (ODbL). Wyszukiwanie: Nominatim.");
                        }
                    });
                });
            });
            ui.add_space(10.0);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                save = ui.add(theme::primary("Zapisz")).clicked();
                if ui.button("Anuluj").clicked() {
                    close = true;
                }
            });
        });
        if save {
            let next = self.settings_open.take().unwrap().draft;
            self.apply_settings(next);
        } else if close || modal.should_close() {
            self.settings_open = None;
        }
        if check {
            self.check_updates(true);
        }
    }

    fn moving_overlay(&mut self, ctx: &egui::Context) {
        let Some(m) = &self.moving else { return };
        let done = m.result.lock().unwrap().take();
        egui::Window::new("Przenoszenie danych")
            .collapsible(false)
            .resizable(false)
            .title_bar(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                ui.set_width(460.0);
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(format!("Przenoszenie danych do {}…", m.to.display()));
                });
            });
        ctx.request_repaint_after(Duration::from_millis(200));
        if let Some(r) = done {
            let m = self.moving.take().unwrap();
            match r {
                Ok(()) => self.commit_settings(m.settings),
                Err(e) => {
                    rfd::MessageDialog::new().set_title("Polskie Studio Map").set_level(rfd::MessageLevel::Error).set_description(format!("Nie udało się przenieść danych:\n{e}\n\nDane, których nie przeniesiono, zostały w dotychczasowym folderze.")).show();
                }
            }
        }
    }

    /// Sprawdzenie aktualizacji w tle; `manual` – komunikat także gdy brak nowszej wersji.
    fn check_updates(&mut self, manual: bool) {
        if matches!(self.update, Update::Checking(..) | Update::Downloading(..)) {
            return;
        }
        let slot = Arc::new(Mutex::new(None));
        let s = slot.clone();
        thread::spawn(move || {
            *s.lock().unwrap() = Some(updater::latest());
        });
        self.update = Update::Checking(slot, manual);
    }

    fn offer_update(&mut self, rel: updater::Release) {
        let size = if rel.size > 0 { format!(" ({} MB)", rel.size / (1 << 20)) } else { String::new() };
        let ok = rfd::MessageDialog::new()
            .set_title("Polskie Studio Map – aktualizacja")
            .set_description(format!(
                "Dostępna jest nowa wersja {}. Masz wersję {}.\n\nPobrać i zainstalować aktualizację{size}? Dane map i ustawienia zostaną zachowane. Instalacja wymaga potwierdzenia uprawnień administratora.",
                rel.version,
                updater::current()
            ))
            .set_buttons(rfd::MessageButtons::YesNo)
            .show();
        if ok != rfd::MessageDialogResult::Yes {
            self.update = Update::Available(rel);
            return;
        }
        let progress = Arc::new(AtomicU64::new(0));
        let slot = Arc::new(Mutex::new(None));
        let (p, s, r) = (progress.clone(), slot.clone(), rel.clone());
        thread::spawn(move || {
            *s.lock().unwrap() = Some(updater::download(&r, p));
        });
        self.update = Update::Downloading(rel, progress, slot);
    }

    fn update_ui(&mut self, ctx: &egui::Context) {
        let mut next = None;
        match &self.update {
            Update::Checking(slot, manual) => {
                let manual = *manual;
                let r = slot.lock().unwrap().take();
                match r {
                    Some(Ok(rel)) if updater::cmp_version(&rel.version, updater::current()) > 0 => {
                        next = Some(if manual {
                            Err(rel)
                        } else {
                            self.atlas.notify(format!("Dostępna jest nowa wersja {} – przycisk „Aktualizacja” u góry.", rel.version));
                            Ok(Update::Available(rel))
                        });
                    }
                    Some(Ok(_)) => {
                        if manual {
                            self.atlas.notify(format!("Masz najnowszą wersję ({}).", updater::current()));
                        }
                        next = Some(Ok(Update::Idle));
                    }
                    Some(Err(e)) => {
                        if manual {
                            self.atlas.notify(format!("Nie udało się sprawdzić aktualizacji: {e}"));
                        } else {
                            log::warn!("aktualizacje: {e}");
                        }
                        next = Some(Ok(Update::Idle));
                    }
                    None => ctx.request_repaint_after(Duration::from_millis(300)),
                }
            }
            Update::Downloading(rel, progress, slot) => {
                let got = progress.load(Ordering::Relaxed);
                let total = rel.size.max(1);
                let version = rel.version.clone();
                egui::Window::new("Aktualizacja")
                    .collapsible(false)
                    .resizable(false)
                    .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                    .show(ctx, |ui| {
                        ui.set_width(380.0);
                        ui.label(format!("Pobieranie wersji {version}…"));
                        theme::bar(ui, got as f32 / total as f32, theme::BRASS);
                        ui.label(RichText::new(format!("{} z {}", fmt_bytes(got), fmt_bytes(total))).small().color(theme::PAPER_3));
                    });
                ctx.request_repaint_after(Duration::from_millis(200));
                let done = slot.lock().unwrap().take();
                if let Some(r) = done {
                    next = Some(Ok(Update::Idle));
                    match r.and_then(|p| updater::run_installer(&p)) {
                        Ok(()) => ctx.send_viewport_cmd(egui::ViewportCommand::Close),
                        Err(e) => {
                            rfd::MessageDialog::new()
                                .set_title("Polskie Studio Map")
                                .set_level(rfd::MessageLevel::Error)
                                .set_description(format!("Nie udało się zainstalować aktualizacji: {e}"))
                                .show();
                        }
                    }
                }
            }
            _ => {}
        }
        match next {
            Some(Ok(u)) => self.update = u,
            Some(Err(rel)) => {
                self.update = Update::Idle;
                self.offer_update(rel);
            }
            None => {}
        }
    }

    fn capture_frame(&mut self, ctx: &egui::Context) {
        let Some((path, at, after, sent)) = &mut self.capture else {
            return;
        };
        ctx.request_repaint_after(Duration::from_millis(100));
        if !*sent && at.elapsed().as_secs_f32() > *after {
            ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(Default::default()));
            *sent = true;
        }
        let shot = ctx.input(|i| {
            i.events.iter().find_map(|e| {
                if let egui::Event::Screenshot { image, .. } = e {
                    Some(image.clone())
                } else {
                    None
                }
            })
        });
        if let Some(img) = shot {
            let [w, h] = img.size;
            let raw: Vec<u8> = img.pixels.iter().flat_map(|c| c.to_array()).collect();
            if let Some(buf) = image::RgbaImage::from_raw(w as u32, h as u32, raw) {
                if let Err(e) = buf.save(&*path) {
                    log::error!("zrzut: {e}");
                }
            }
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
    }
}

impl eframe::App for App {
    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        eframe::set_value(storage, PREFS_KEY, &self.atlas.save_prefs());
    }

    fn update(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
        self.capture_frame(ctx);
        if ctx.input_mut(|i| i.consume_key(egui::Modifiers::CTRL, egui::Key::Comma)) {
            self.open_settings(Page::General);
        }
        if self.last_gc.elapsed() > Duration::from_secs(120) {
            self.collect_garbage();
        }
        let r = std::mem::take(&mut self.atlas.requests);
        if r.open_settings {
            self.open_settings(Page::Data);
        }
        if r.check_updates {
            self.check_updates(true);
        }
        self.update_ui(ctx);

        crate::ui::panels::sidebar(&mut self.atlas, ctx);
        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(theme::INK_0))
            .show(ctx, |ui| {
                self.atlas.view_ui(ui, frame);
            });
        crate::ui::panels::search_pill(&mut self.atlas, ctx);
        crate::ui::panels::rail(&mut self.atlas, ctx);
        crate::ui::panels::nav(&mut self.atlas, ctx);
        crate::ui::panels::layers_panel(&mut self.atlas, ctx);
        crate::ui::panels::stats_panel(&mut self.atlas, ctx);
        crate::ui::panels::weather_panel(&mut self.atlas, ctx);
        self.top_right(ctx);
        crate::ui::search::show(&mut self.atlas, ctx);
        crate::ui::panels::help(&mut self.atlas, ctx);
        self.settings_window(ctx);
        self.moving_overlay(ctx);
    }
}

/// Przeniesienie zawartości folderu: zmiana nazwy na tym samym dysku, kopia + usunięcie między dyskami.
fn move_dir(from: &Path, to: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(to)?;
    for e in std::fs::read_dir(from)? {
        let e = e?;
        let target = to.join(e.file_name());
        if std::fs::rename(e.path(), &target).is_err() {
            copy_tree(&e.path(), &target)?;
            if e.file_type()?.is_dir() {
                std::fs::remove_dir_all(e.path())?;
            } else {
                std::fs::remove_file(e.path())?;
            }
        }
    }
    let _ = std::fs::remove_dir_all(from);
    Ok(())
}

fn copy_tree(from: &Path, to: &Path) -> std::io::Result<()> {
    if from.is_dir() {
        std::fs::create_dir_all(to)?;
        for e in std::fs::read_dir(from)? {
            let e = e?;
            copy_tree(&e.path(), &to.join(e.file_name()))?;
        }
        Ok(())
    } else {
        std::fs::copy(from, to).map(|_| ())
    }
}
