//! Wszystkie polecenia programu w jednym rejestrze (jak w GRANI): nazwa, ikona, grupa, skrót, stan.
//! Z rejestru korzystają skróty klawiszowe ([`Action::for_key`]), paleta poleceń (Ctrl+K) i okno
//! skrótów, więc nowe polecenie dopisane tutaj od razu działa we wszystkich trzech miejscach.

use super::*;
use crate::layers::Kind;
use crate::ui::panels::Tab;

/// Skrót: klawisz z modyfikatorami.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Keys {
    pub key: egui::Key,
    pub ctrl: bool,
}

const fn k(key: egui::Key) -> Option<Keys> {
    Some(Keys { key, ctrl: false })
}

const fn ctrl(key: egui::Key) -> Option<Keys> {
    Some(Keys { key, ctrl: true })
}

impl Keys {
    /// Zapis skrótu do wyświetlenia, np. „Ctrl+K”.
    pub fn text(&self) -> String {
        let name = match self.key {
            egui::Key::Equals => "=",
            egui::Key::Minus => "−",
            egui::Key::OpenBracket => "[",
            egui::Key::CloseBracket => "]",
            egui::Key::Slash => "/",
            egui::Key::Comma => ",",
            egui::Key::Backspace => "⌫",
            k => k.symbol_or_name(),
        };
        if self.ctrl {
            format!("Ctrl+{name}")
        } else {
            name.to_string()
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Group {
    Tools,
    View,
    Global,
    Objects,
    Panels,
    Interface,
}

impl Group {
    pub const ALL: [Group; 6] = [Group::Tools, Group::View, Group::Global, Group::Objects, Group::Panels, Group::Interface];

    pub fn label(self) -> &'static str {
        match self {
            Group::Tools => "Narzędzia",
            Group::View => "Widok",
            Group::Global => "Warstwy globalne",
            Group::Objects => "Obiekty na mapie",
            Group::Panels => "Panele i okna",
            Group::Interface => "Program",
        }
    }
}

/// Wbudowane składniki mapy (przełączniki w panelu warstw), podzielone jak warstwy z katalogu.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BaseLayer {
    Labels,
    Trails,
    Contours,
    Grid,
    Shadows,
    Detail,
    Snow,
    Ortho,
    Landcover,
    Surface,
}

impl BaseLayer {
    pub const ALL: [BaseLayer; 10] = [
        BaseLayer::Labels,
        BaseLayer::Trails,
        BaseLayer::Contours,
        BaseLayer::Grid,
        BaseLayer::Ortho,
        BaseLayer::Shadows,
        BaseLayer::Detail,
        BaseLayer::Landcover,
        BaseLayer::Snow,
        BaseLayer::Surface,
    ];

    /// Rodzaj: barwi całą mapę albo pokazuje obiekty.
    pub fn kind(self) -> Kind {
        match self {
            BaseLayer::Labels | BaseLayer::Trails | BaseLayer::Contours | BaseLayer::Grid => Kind::Objects,
            _ => Kind::Global,
        }
    }

    pub fn on(self, p: &Prefs) -> bool {
        match self {
            BaseLayer::Labels => p.labels,
            BaseLayer::Trails => p.trails,
            BaseLayer::Contours => p.contours,
            BaseLayer::Grid => p.km_grid,
            BaseLayer::Shadows => p.shadows,
            BaseLayer::Detail => p.detail,
            BaseLayer::Snow => p.snow,
            BaseLayer::Ortho => p.ortho,
            BaseLayer::Landcover => p.landcover,
            BaseLayer::Surface => p.surface,
        }
    }

    /// Ikona, nazwa i skrót.
    pub fn info(self) -> (&'static str, &'static str, Option<Keys>) {
        use egui::Key as K;
        use theme::icon as i;
        match self {
            BaseLayer::Labels => (i::TEXT_AA, "Nazwy miejsc", k(K::L)),
            BaseLayer::Trails => (i::PATH, "Szlaki", k(K::T)),
            BaseLayer::Contours => (i::WAVE_SINE, "Poziomice", k(K::C)),
            BaseLayer::Grid => (i::GRID_FOUR, "Siatka kilometrowa", k(K::U)),
            BaseLayer::Shadows => (i::CLOUD_SUN, "Cienie rzeźby", None),
            BaseLayer::Detail => (i::DROP_HALF, "Drobna rzeźba", None),
            BaseLayer::Snow => (i::SNOWFLAKE, "Śnieg", None),
            BaseLayer::Ortho => (i::IMAGE, "Ortofoto GUGiK", None),
            BaseLayer::Landcover => (i::TREE_EVERGREEN, "Lasy i woda (OSM)", None),
            BaseLayer::Surface => (i::BUILDINGS, "Drzewa i budynki (NMPT)", None),
        }
    }

    /// Dopisek w podpowiedzi kafelka.
    pub fn hint(self) -> &'static str {
        match self {
            BaseLayer::Labels => "Miejscowości, szczyty, przełęcze i schroniska z OSM",
            BaseLayer::Trails => "Szlaki turystyczne z OSM w kolorach znakowania",
            BaseLayer::Contours => "Linie równych wysokości z modelu terenu",
            BaseLayer::Grid => "Siatka co 1 km (z daleka co 10 km)",
            BaseLayer::Shadows => "Cienie gór zgodne z porą dnia",
            BaseLayer::Detail => "Mapa normalnych: drobna rzeźba z bliska",
            BaseLayer::Snow => "Śnieg powyżej granicy wieloletniej dla pory roku (styl Teren)",
            BaseLayer::Ortho => "Ortofotomapa GUGiK z bliska w stylu Satelita",
            BaseLayer::Landcover => "Wody, lasy i skały z OSM w stylach Teren i Mapa",
            BaseLayer::Surface => "Numeryczny model pokrycia terenu z lidaru – drzewa i budynki z bliska",
        }
    }

    /// Powód, dla którego składnik nic teraz nie zmienia (kafelek szary).
    pub fn off_reason(self, p: &Prefs) -> Option<&'static str> {
        match self {
            BaseLayer::Snow if p.style != Style::Terrain => Some("widać tylko w stylu Teren"),
            BaseLayer::Ortho if p.style != Style::Satellite => Some("widać tylko w stylu Satelita"),
            BaseLayer::Landcover if !matches!(p.style, Style::Terrain | Style::Map) => Some("widać w stylach Teren i Mapa"),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    Tool(Tool),
    Lens(Lens),
    Panorama,
    North,
    TopView,
    ResetView,
    Fullscreen,
    NextStyle,
    Style(Style),
    ExagUp,
    ExagDown,
    HourBack,
    HourForward,
    Now,
    Base(BaseLayer),
    /// Wyłącza wszystkie warstwy danego rodzaju.
    AllOff(Kind),
    Legend,
    Tab(Tab),
    Panel,
    Search,
    Palette,
    Help,
    Settings,
    SaveView,
    AddPlace,
    CopyLink,
    CopyCoords,
    Screenshot,
    DropLast,
    CheckUpdates,
}

impl Action {
    /// Wszystkie polecenia w kolejności palety.
    pub fn all() -> Vec<Action> {
        let mut v = vec![Action::Tool(Tool::Explore), Action::Tool(Tool::Measure)];
        v.extend([Lens::Band, Lens::Rel, Lens::Vis].map(Action::Lens));
        v.extend([Action::Panorama, Action::DropLast]);
        v.extend([Action::North, Action::TopView, Action::ResetView, Action::Fullscreen, Action::NextStyle]);
        v.extend(Style::ALL.map(Action::Style));
        v.extend([Action::ExagUp, Action::ExagDown, Action::HourBack, Action::HourForward, Action::Now]);
        v.extend(BaseLayer::ALL.iter().filter(|b| b.kind() == Kind::Global).map(|b| Action::Base(*b)));
        v.push(Action::AllOff(Kind::Global));
        v.extend(BaseLayer::ALL.iter().filter(|b| b.kind() == Kind::Objects).map(|b| Action::Base(*b)));
        v.push(Action::AllOff(Kind::Objects));
        v.push(Action::Legend);
        v.extend(Tab::ALL.map(Action::Tab));
        v.extend([Action::Panel, Action::Search, Action::Palette, Action::Help, Action::Settings]);
        v.extend([Action::SaveView, Action::AddPlace, Action::CopyLink, Action::CopyCoords, Action::Screenshot, Action::CheckUpdates]);
        v
    }

    /// Ikona, nazwa i grupa.
    pub fn info(self) -> (&'static str, String, Group) {
        use theme::icon as i;
        let s = |t: &str| t.to_string();
        match self {
            Action::Tool(Tool::Explore) => (i::HAND_POINTING, s("Eksploracja (bez narzędzia)"), Group::Tools),
            Action::Tool(Tool::Measure) => (i::RULER, s("Pomiar odległości, pola i linii wzroku"), Group::Tools),
            Action::Lens(l) => {
                let (g, t) = match l {
                    Lens::Band => (i::WAVES, "Soczewka: ta sama wysokość"),
                    Lens::Rel => (i::PLUS_MINUS, "Soczewka: wysokość względna"),
                    Lens::Vis => (i::EYE, "Soczewka: widoczność"),
                    Lens::None => (i::EYE_SLASH, "Bez soczewki"),
                };
                (g, s(t), Group::Tools)
            }
            Action::Panorama => (i::MOUNTAINS, s("Panorama ze środka widoku"), Group::Tools),
            Action::DropLast => (i::BACKSPACE, s("Usuń ostatni punkt pomiaru"), Group::Tools),
            Action::North => (i::COMPASS, s("Północ u góry"), Group::View),
            Action::TopView => (i::ARROW_FAT_DOWN, s("Widok z góry / ukośny"), Group::View),
            Action::ResetView => (i::ARROW_COUNTER_CLOCKWISE, s("Domyślne nachylenie widoku"), Group::View),
            Action::Fullscreen => (i::CORNERS_OUT, s("Pełny ekran"), Group::View),
            Action::NextStyle => (i::PAINT_BRUSH, s("Następny styl mapy"), Group::View),
            Action::Style(st) => (crate::ui::panels::style_glyph(st), format!("Styl mapy: {}", st.label()), Group::Global),
            Action::ExagUp => (i::ARROW_UP, s("Większe przewyższenie terenu"), Group::View),
            Action::ExagDown => (i::ARROW_DOWN, s("Mniejsze przewyższenie terenu"), Group::View),
            Action::HourBack => (i::CLOCK_COUNTER_CLOCKWISE, s("Pół godziny wcześniej"), Group::View),
            Action::HourForward => (i::CLOCK_CLOCKWISE, s("Pół godziny później"), Group::View),
            Action::Now => (i::CLOCK, s("Bieżąca pora dnia"), Group::View),
            Action::Base(b) => {
                let (g, t, _) = b.info();
                (g, s(t), if b.kind() == Kind::Global { Group::Global } else { Group::Objects })
            }
            Action::AllOff(Kind::Global) => (i::EYE_SLASH, s("Wyłącz wszystkie warstwy globalne"), Group::Global),
            Action::AllOff(Kind::Objects) => (i::EYE_SLASH, s("Wyłącz wszystkie obiekty"), Group::Objects),
            Action::Legend => (i::LIST_BULLETS, s("Legenda na mapie"), Group::View),
            Action::Tab(t) => (t.icon(), format!("Panel: {}", t.label()), Group::Panels),
            Action::Panel => (i::SIDEBAR_SIMPLE, s("Pokaż / ukryj panel boczny"), Group::Panels),
            Action::Search => (i::MAGNIFYING_GLASS, s("Szukaj miejsca, adresu, działki, współrzędnych"), Group::Panels),
            Action::Palette => (i::COMMAND, s("Paleta poleceń"), Group::Panels),
            Action::Help => (i::QUESTION, s("Skróty i sterowanie"), Group::Panels),
            Action::Settings => (i::GEAR, s("Ustawienia"), Group::Panels),
            Action::SaveView => (i::BOOKMARK_SIMPLE, s("Zapisz widok z warstwami w bibliotece"), Group::Interface),
            Action::AddPlace => (i::STAR, s("Dodaj środek widoku do ulubionych"), Group::Interface),
            Action::CopyLink => (i::LINK, s("Kopiuj link do widoku"), Group::Interface),
            Action::CopyCoords => (i::COPY, s("Kopiuj współrzędne środka widoku"), Group::Interface),
            Action::Screenshot => (i::CAMERA, s("Zrzut ekranu PNG"), Group::Interface),
            Action::CheckUpdates => (i::ARROW_CIRCLE_UP, s("Sprawdź aktualizacje"), Group::Interface),
        }
    }

    /// Skrót klawiszowy polecenia.
    pub fn keys(self) -> Option<Keys> {
        use egui::Key as K;
        match self {
            Action::Tool(Tool::Explore) => k(K::Num1),
            Action::Tool(Tool::Measure) => k(K::Num2),
            Action::Lens(Lens::Band) => k(K::H),
            Action::Lens(Lens::Rel) => k(K::W),
            Action::Lens(Lens::Vis) => k(K::O),
            Action::Lens(Lens::None) => None,
            Action::DropLast => k(K::Backspace),
            Action::North => k(K::N),
            Action::TopView => k(K::V),
            Action::ResetView => k(K::R),
            Action::Fullscreen => k(K::F11),
            Action::NextStyle => k(K::M),
            Action::ExagUp => k(K::Equals),
            Action::ExagDown => k(K::Minus),
            Action::HourBack => k(K::OpenBracket),
            Action::HourForward => k(K::CloseBracket),
            Action::Base(b) => b.info().2,
            Action::Legend => k(K::J),
            Action::Tab(t) => t.keys(),
            Action::Panel => k(K::P),
            Action::Search => k(K::Slash),
            Action::Palette => ctrl(K::K),
            Action::Help => k(K::F1),
            Action::Settings => ctrl(K::Comma),
            Action::SaveView => ctrl(K::D),
            Action::CopyLink => ctrl(K::L),
            Action::Screenshot => k(K::S),
            _ => None,
        }
    }

    /// Polecenie przypisane do naciśniętego klawisza.
    pub fn for_key(key: egui::Key, m: egui::Modifiers) -> Option<Action> {
        use egui::Key as K;
        let c = m.ctrl || m.command;
        // zapasowe skróty, których nie pokazujemy osobno
        match (key, c) {
            (K::Plus, false) => return Some(Action::ExagUp),
            (K::Delete, false) => return Some(Action::DropLast),
            (K::I, false) => return Some(Action::Lens(Lens::Band)),
            (K::P, true) if m.shift => return Some(Action::Palette),
            // „?” (Shift+/) otwiera pomoc – obsługiwane osobno jako znak
            (K::Slash, false) if m.shift => return None,
            _ => {}
        }
        Action::all().into_iter().find(|a| a.keys().is_some_and(|b| b.key == key && b.ctrl == c))
    }

    /// Stan przełącznika (`None` – zwykłe polecenie).
    pub fn checked(self, a: &Atlas) -> Option<bool> {
        let p = &a.prefs;
        Some(match self {
            Action::Tool(t) => a.tool == t,
            Action::Lens(l) => a.lens == l,
            Action::Panorama => a.panorama.is_some(),
            Action::Style(s) => p.style == s,
            Action::Base(b) => b.on(p),
            Action::Legend => p.legend,
            Action::Tab(t) => p.panel_open && a.tab == t,
            Action::Panel => p.panel_open,
            _ => return None,
        })
    }

    /// Czy polecenie ma teraz sens (pozostałe są wyszarzone w palecie).
    pub fn enabled(self, a: &Atlas) -> bool {
        match self {
            Action::DropLast => a.tool == Tool::Measure && !a.measure.is_empty(),
            Action::AllOff(k) => a.count_on(k) > 0,
            _ => true,
        }
    }
}

impl Atlas {
    /// Wykonuje polecenie z rejestru.
    pub fn run(&mut self, act: Action, ctx: &egui::Context) {
        match act {
            Action::Tool(Tool::Explore) => {
                self.tool = Tool::Explore;
                if self.lens_pin.is_some() {
                    self.unpin_lens();
                }
                self.lens = Lens::None;
                if self.panorama.is_some() {
                    self.exit_panorama();
                }
            }
            Action::Tool(Tool::Measure) => {
                if self.tool == Tool::Measure {
                    self.tool = Tool::Explore;
                } else {
                    self.tool = Tool::Measure;
                    self.prefs.panel_open = true;
                    self.tab = Tab::Info;
                }
            }
            Action::Lens(l) => self.set_lens(l),
            Action::Panorama => {
                if self.panorama.is_some() {
                    self.exit_panorama();
                } else {
                    let (lon, lat) = (self.cam.lon, self.cam.lat);
                    self.enter_panorama(lon, lat);
                }
            }
            Action::DropLast => {
                if self.tool == Tool::Measure && !self.measure.is_empty() {
                    self.measure.pop();
                    self.start_los();
                }
            }
            Action::North => {
                let mut to = self.cam;
                to.heading = 0.0;
                self.start_fly(to, 0.7);
            }
            Action::TopView => {
                let mut to = self.cam;
                to.pitch = if self.cam.pitch > 75.0 { 40.0 } else { 89.0 };
                self.start_fly(to, 0.7);
            }
            Action::ResetView => {
                let d = Camera::default();
                let mut to = self.cam;
                to.heading = d.heading;
                to.pitch = d.pitch;
                to.distance = d.distance.min(self.cam.distance.max(8000.0));
                self.start_fly(to, 0.9);
            }
            Action::Fullscreen => {
                let full = ctx.input(|i| i.viewport().fullscreen.unwrap_or(false));
                ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(!full));
            }
            Action::NextStyle => {
                let all = Style::ALL;
                let i = all.iter().position(|s| *s == self.prefs.style).unwrap_or(0);
                self.prefs.style = all[(i + 1) % all.len()];
            }
            Action::Style(s) => self.prefs.style = s,
            Action::ExagUp => self.prefs.exag = (((self.prefs.exag + 0.1) * 10.0).round() / 10.0).min(3.0),
            Action::ExagDown => self.prefs.exag = (((self.prefs.exag - 0.1) * 10.0).round() / 10.0).max(1.0),
            Action::HourBack => self.shift_hour(-0.5),
            Action::HourForward => self.shift_hour(0.5),
            Action::Now => self.prefs.custom_time = None,
            Action::Base(b) => {
                let on = !b.on(&self.prefs);
                self.set_base(b, on);
            }
            Action::AllOff(k) => self.all_off(k),
            Action::Legend => self.prefs.legend = !self.prefs.legend,
            Action::Tab(t) => crate::ui::panels::toggle_tab(self, t),
            Action::Panel => self.prefs.panel_open = !self.prefs.panel_open,
            Action::Search => self.search = Some(crate::ui::search::Search::default()),
            Action::Palette => self.palette = Some(Default::default()),
            Action::Help => self.help = !self.help,
            Action::Settings => self.requests.open_general = true,
            Action::SaveView => self.save_view(),
            Action::AddPlace => {
                let (lon, lat) = (self.cam.lon, self.cam.lat);
                let name = self.name_near(lon, lat, 1500.0).unwrap_or_else(|| "Miejsce".to_string());
                self.add_place(&name, lon, lat);
            }
            Action::CopyLink => {
                ctx.copy_text(self.share_link());
                self.notify("Skopiowano link do widoku – wklej go w wyszukiwarkę programu.");
            }
            Action::CopyCoords => {
                ctx.copy_text(crate::coords::format_pos(self.cam.lon, self.cam.lat));
                self.notify("Skopiowano współrzędne środka widoku.");
            }
            Action::Screenshot => self.screenshot(ctx),
            Action::CheckUpdates => self.requests.check_updates = true,
        }
        self.prefs.exag = self.prefs.exag.clamp(1.0, 3.0);
    }

    /// Włącza albo wyłącza wbudowany składnik mapy (z jego skutkami ubocznymi).
    pub fn set_base(&mut self, b: BaseLayer, on: bool) {
        let p = &mut self.prefs;
        match b {
            BaseLayer::Labels => p.labels = on,
            BaseLayer::Trails => p.trails = on,
            BaseLayer::Contours => p.contours = on,
            BaseLayer::Grid => p.km_grid = on,
            BaseLayer::Shadows => p.shadows = on,
            BaseLayer::Detail => p.detail = on,
            BaseLayer::Snow => p.snow = on,
            BaseLayer::Ortho => p.ortho = on,
            BaseLayer::Landcover => p.landcover = on,
            BaseLayer::Surface => {
                if p.surface != on {
                    p.surface = on;
                    crate::download::set_surface(on);
                    // ten sam kafel ma teraz inne wysokości
                    self.heights.set_store(self.heights.store());
                    if let Some(r) = &mut self.renderer {
                        r.tiles.reset();
                    }
                }
            }
        }
    }

    /// Liczba włączonych warstw danego rodzaju (z katalogu i dodatkowych: pogoda, kartogram, na żywo).
    pub fn count_on(&self, k: Kind) -> usize {
        let cat = self.prefs.layers.iter().filter(|l| l.on && crate::layers::index_of(&l.id).is_some_and(|i| crate::layers::kind_of(i) == k)).count();
        match k {
            Kind::Global => cat + self.weather_field.is_some() as usize + self.satellite as usize + crate::stats::map().is_some() as usize,
            Kind::Objects => cat + self.prefs.feeds.len() + self.wind_particles as usize + self.stats_bars as usize,
        }
    }

    /// Wyłącza wszystkie warstwy z katalogu danego rodzaju i dodatki tego rodzaju (wbudowane
    /// składniki mapy zostają – to podkład).
    pub fn all_off(&mut self, k: Kind) {
        for l in &mut self.prefs.layers {
            if crate::layers::index_of(&l.id).is_some_and(|i| crate::layers::kind_of(i) == k) {
                l.on = false;
            }
        }
        match k {
            Kind::Global => {
                self.weather_field = None;
                self.satellite = false;
                crate::stats::clear_map();
            }
            Kind::Objects => {
                self.prefs.feeds.clear();
                self.wind_particles = false;
                self.stats_bars = false;
            }
        }
    }
}
