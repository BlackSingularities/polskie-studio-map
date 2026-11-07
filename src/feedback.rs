//! Zgłoszenia do redzikowski.com (wspólny moduł aplikacji egui):
//!  - raport o błędzie – panika zapisana przez [`install`] i wysłana przy następnym
//!    starcie, jeśli użytkownik się zgodzi (trafia do „Raportów błędów” w panelu),
//!  - zgłoszenie błędu albo sugestia funkcjonalności pisane ręcznie (trafia do
//!    „Sugestii i zgłoszeń”).
//!
//! Adres e-mail jest zawsze opcjonalny. Nic nie wychodzi z komputera bez akcji
//! użytkownika. Plik jest taki sam we wszystkich aplikacjach egui – zmieniaj go
//! razem we wszystkich.

use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use eframe::egui;

const ENDPOINT: &str = "https://redzikowski.com/api/feedback";
const CRASH_FILE: &str = "feedback-pending-crash.json";

struct Config {
    slug: &'static str,
    name: &'static str,
    version: &'static str,
    dir: PathBuf,
}

static CONFIG: OnceLock<Config> = OnceLock::new();

/// Wywołaj raz na początku `main`. `dir_name` – folder w %LOCALAPPDATA% na plik awarii.
pub fn install(slug: &'static str, name: &'static str, version: &'static str, dir_name: &str) {
    let base = std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    let dir = base.join(dir_name);
    if CONFIG.set(Config { slug, name, version, dir }).is_err() {
        return;
    }
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = save_crash(info);
        previous(info);
    }));
}

fn config() -> Option<&'static Config> {
    CONFIG.get()
}

fn crash_path() -> Option<PathBuf> {
    config().map(|c| c.dir.join(CRASH_FILE))
}

fn save_crash(info: &std::panic::PanicHookInfo<'_>) -> std::io::Result<()> {
    let Some(path) = crash_path() else { return Ok(()) };
    let message = info
        .payload()
        .downcast_ref::<&str>()
        .map(|s| s.to_string())
        .or_else(|| info.payload().downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "panic".into());
    let location = info.location().map(|l| format!("{}:{}:{}", l.file(), l.line(), l.column())).unwrap_or_default();
    let thread = std::thread::current().name().unwrap_or("?").to_string();
    let backtrace = std::backtrace::Backtrace::force_capture().to_string();
    let stack = format!("panic at {location}: {message}\nthread: {thread}\n\n{backtrace}");
    let json = serde_json::json!({
        "stack": stack.chars().take(60_000).collect::<String>(),
        "at": now_ms(),
        "thread": thread,
        "appVersion": config().map(|c| c.version).unwrap_or(""),
    });
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, json.to_string())
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

#[derive(Clone)]
pub struct PendingCrash {
    pub stack: String,
    pub app_version: String,
    pub thread: String,
    pub at: u64,
}

pub fn pending_crash() -> Option<PendingCrash> {
    let text = std::fs::read_to_string(crash_path()?).ok()?;
    let v: serde_json::Value = serde_json::from_str(&text).ok()?;
    Some(PendingCrash {
        stack: v["stack"].as_str().unwrap_or("").to_string(),
        app_version: v["appVersion"].as_str().unwrap_or("").to_string(),
        thread: v["thread"].as_str().unwrap_or("").to_string(),
        at: v["at"].as_u64().unwrap_or(0),
    })
}

pub fn discard_pending_crash() {
    if let Some(p) = crash_path() {
        let _ = std::fs::remove_file(p);
    }
}

fn os_name() -> String {
    // Numer kompilacji z rejestru wymagałby dodatkowych zależności – wystarcza rodzina i architektura.
    format!("{} {}", std::env::consts::OS, std::env::consts::ARCH)
}

fn locale() -> String {
    std::env::var("LANG").ok().filter(|s| !s.is_empty()).unwrap_or_default()
}

fn base(include_device: bool) -> serde_json::Map<String, serde_json::Value> {
    let c = config();
    let mut m = serde_json::Map::new();
    m.insert("app".into(), c.map(|c| c.slug).unwrap_or("").into());
    m.insert("appName".into(), c.map(|c| c.name).unwrap_or("").into());
    m.insert("appVersion".into(), c.map(|c| c.version).unwrap_or("").into());
    m.insert("platform".into(), if cfg!(target_os = "windows") { "windows" } else if cfg!(target_os = "macos") { "macos" } else { "linux" }.into());
    m.insert("locale".into(), locale().into());
    if include_device {
        m.insert("os".into(), os_name().into());
        m.insert(
            "extra".into(),
            serde_json::json!({ "cpus": std::thread::available_parallelism().map(|n| n.get()).unwrap_or(0) }),
        );
    }
    m
}

fn post(body: serde_json::Map<String, serde_json::Value>) -> Result<(), String> {
    let agent = ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(10))
        .timeout(Duration::from_secs(20))
        .build();
    let text = serde_json::Value::Object(body).to_string();
    match agent.post(ENDPOINT).set("Content-Type", "application/json; charset=utf-8").send_string(&text) {
        Ok(_) => Ok(()),
        Err(ureq::Error::Status(429, _)) => Err("Za dużo zgłoszeń – spróbuj później.".into()),
        Err(ureq::Error::Status(code, _)) => Err(format!("Serwer odrzucił zgłoszenie (HTTP {code}).")),
        Err(e) => Err(format!("Brak połączenia z redzikowski.com: {e}")),
    }
}

pub fn send_crash(crash: &PendingCrash, comment: &str, email: &str) -> Result<(), String> {
    let mut b = base(true);
    b.insert("kind".into(), "crash".into());
    if !crash.app_version.is_empty() {
        b.insert("appVersion".into(), crash.app_version.clone().into());
    }
    b.insert("stack".into(), crash.stack.clone().into());
    b.insert("message".into(), comment.trim().into());
    b.insert("email".into(), email.trim().into());
    if let Some(serde_json::Value::Object(extra)) = b.get_mut("extra") {
        extra.insert("thread".into(), crash.thread.clone().into());
        extra.insert("crashedAt".into(), crash.at.into());
    }
    post(b).inspect(|_| discard_pending_crash())
}

pub fn send(feature: bool, message: &str, email: &str, include_device: bool) -> Result<(), String> {
    let mut b = base(include_device);
    b.insert("kind".into(), if feature { "feature" } else { "bug" }.into());
    b.insert("message".into(), message.trim().into());
    b.insert("email".into(), email.trim().into());
    post(b)
}

fn email_ok(email: &str) -> bool {
    let e = email.trim();
    e.is_empty() || (e.contains('@') && e.rsplit('@').next().is_some_and(|d| d.contains('.')) && !e.contains(' '))
}

/* ── okna ─────────────────────────────────────────────────────────────── */

type Job = Arc<Mutex<Option<Result<(), String>>>>;

#[derive(Default)]
enum Mode {
    #[default]
    Closed,
    Form,
    Crash(PendingCrash),
}

/// Stan okien zgłoszeń. Trzymaj w strukturze aplikacji i wołaj [`FeedbackUi::show`] w każdej klatce.
#[derive(Default)]
pub struct FeedbackUi {
    mode: Mode,
    feature: bool,
    message: String,
    email: String,
    include_device: bool,
    job: Option<Job>,
    error: Option<String>,
    sent: bool,
    checked_crash: bool,
}

impl FeedbackUi {
    /// Formularz „Zgłoś błąd lub sugestię”.
    pub fn open(&mut self) {
        self.mode = Mode::Form;
        self.message.clear();
        self.feature = false;
        self.include_device = true;
        self.error = None;
        self.sent = false;
        self.job = None;
    }

    pub fn show(&mut self, ctx: &egui::Context) {
        if !self.checked_crash {
            self.checked_crash = true;
            if let Some(crash) = pending_crash() {
                self.mode = Mode::Crash(crash);
                self.message.clear();
                self.error = None;
                self.sent = false;
            }
        }
        if let Some(job) = &self.job {
            let done = job.lock().ok().and_then(|mut g| g.take());
            match done {
                Some(Ok(())) => {
                    self.job = None;
                    self.sent = true;
                    self.error = None;
                }
                Some(Err(e)) => {
                    self.job = None;
                    self.error = Some(e);
                }
                None => ctx.request_repaint_after(Duration::from_millis(150)),
            }
        }
        match std::mem::take(&mut self.mode) {
            Mode::Closed => {}
            Mode::Form => {
                if self.form(ctx) {
                    self.mode = Mode::Form;
                }
            }
            Mode::Crash(crash) => {
                if self.crash(ctx, &crash) {
                    self.mode = Mode::Crash(crash);
                }
            }
        }
    }

    fn start(&mut self, ctx: &egui::Context, work: impl FnOnce() -> Result<(), String> + Send + 'static) {
        let job: Job = Arc::new(Mutex::new(None));
        let slot = job.clone();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let r = work();
            if let Ok(mut g) = slot.lock() {
                *g = Some(r);
            }
            ctx.request_repaint();
        });
        self.job = Some(job);
        self.error = None;
    }

    fn window(title: &str) -> egui::Window<'_> {
        egui::Window::new(title)
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .default_width(440.0)
    }

    fn email_field(&mut self, ui: &mut egui::Ui) {
        ui.label("Twój e-mail (opcjonalnie – jeśli chcesz odpowiedź)");
        ui.add(egui::TextEdit::singleline(&mut self.email).hint_text("np. jan@example.com").desired_width(f32::INFINITY));
        if !email_ok(&self.email) {
            ui.colored_label(ui.visuals().error_fg_color, "To nie wygląda na adres e-mail.");
        }
    }

    /// Zwraca `true`, gdy okno ma zostać otwarte.
    fn form(&mut self, ctx: &egui::Context) -> bool {
        let mut keep = true;
        let busy = self.job.is_some();
        Self::window("Zgłoś błąd lub sugestię").show(ctx, |ui| {
            ui.set_width(440.0);
            if self.sent {
                ui.label(egui::RichText::new("Dziękuję! Zgłoszenie dotarło.").strong());
                ui.label(if self.email.trim().is_empty() {
                    "Przeczytam je i wezmę pod uwagę przy kolejnych wersjach."
                } else {
                    "Przeczytam je i odezwę się na podany adres, jeśli będzie trzeba."
                });
                ui.add_space(8.0);
                if ui.button("Zamknij").clicked() {
                    keep = false;
                }
                return;
            }
            ui.horizontal(|ui| {
                ui.selectable_value(&mut self.feature, false, "Błąd");
                ui.selectable_value(&mut self.feature, true, "Sugestia funkcji");
            });
            ui.add_space(6.0);
            ui.label(if self.feature {
                "Czego brakuje albo co można zrobić lepiej?"
            } else {
                "Co się stało? Co robiłeś(-aś) tuż przed błędem?"
            });
            ui.add(egui::TextEdit::multiline(&mut self.message).desired_rows(6).desired_width(f32::INFINITY));
            ui.add_space(6.0);
            self.email_field(ui);
            ui.add_space(4.0);
            ui.checkbox(&mut self.include_device, "Dołącz wersję aplikacji i systemu");
            if let Some(e) = &self.error {
                ui.colored_label(ui.visuals().error_fg_color, e);
            }
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                let ready = self.message.trim().chars().count() >= 3 && email_ok(&self.email) && !busy;
                if ui.add_enabled(ready, egui::Button::new(if busy { "Wysyłanie…" } else { "Wyślij" })).clicked() {
                    let (feature, message, email, dev) = (self.feature, self.message.clone(), self.email.clone(), self.include_device);
                    self.start(ctx, move || send(feature, &message, &email, dev));
                }
                if ui.add_enabled(!busy, egui::Button::new("Anuluj")).clicked() {
                    keep = false;
                }
            });
            ui.add_space(4.0);
            ui.label(egui::RichText::new("Zgłoszenie trafi do autora przez redzikowski.com.").small().weak());
        });
        keep
    }

    fn crash(&mut self, ctx: &egui::Context, crash: &PendingCrash) -> bool {
        let mut keep = true;
        let busy = self.job.is_some();
        Self::window("Aplikacja została nieoczekiwanie zamknięta").show(ctx, |ui| {
            ui.set_width(440.0);
            if self.sent {
                ui.label(egui::RichText::new("Dziękuję! Raport został wysłany.").strong());
                ui.add_space(8.0);
                if ui.button("Zamknij").clicked() {
                    keep = false;
                }
                return;
            }
            ui.label("Przy ostatnim uruchomieniu wystąpił błąd. Czy wysłać raport, żeby można go było naprawić? Raport zawiera opis błędu, wersję aplikacji i systemu – żadnych Twoich plików.");
            ui.add_space(6.0);
            egui::CollapsingHeader::new("Pokaż szczegóły").show(ui, |ui| {
                egui::ScrollArea::vertical().max_height(160.0).show(ui, |ui| {
                    ui.label(egui::RichText::new(&crash.stack).monospace().small());
                });
            });
            ui.add_space(6.0);
            ui.label("Co robiłeś(-aś) przed awarią? (opcjonalnie)");
            ui.add(egui::TextEdit::multiline(&mut self.message).desired_rows(3).desired_width(f32::INFINITY));
            ui.add_space(6.0);
            self.email_field(ui);
            if let Some(e) = &self.error {
                ui.colored_label(ui.visuals().error_fg_color, e);
            }
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                if ui
                    .add_enabled(email_ok(&self.email) && !busy, egui::Button::new(if busy { "Wysyłanie…" } else { "Wyślij raport" }))
                    .clicked()
                {
                    let (c, message, email) = (crash.clone(), self.message.clone(), self.email.clone());
                    self.start(ctx, move || send_crash(&c, &message, &email));
                }
                if ui.add_enabled(!busy, egui::Button::new("Nie wysyłaj")).clicked() {
                    discard_pending_crash();
                    keep = false;
                }
            });
        });
        keep
    }
}
