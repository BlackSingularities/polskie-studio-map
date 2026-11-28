//! Biblioteka użytkownika (jak w GRANI): ulubione miejsca i zapisane widoki. Widok pamięta kamerę,
//! styl, przewyższenie i cały stos włączonych warstw (globalnych i obiektów) z kryciem i kolejnością,
//! więc jednym kliknięciem wraca się do gotowego zestawienia map. Plik `%APPDATA%\PSM\biblioteka.json`,
//! zapisywany po każdej zmianie.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::ui::atlas::{LayerState, Style};

/// Ulubione miejsce.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Place {
    pub id: u64,
    pub name: String,
    pub lon: f64,
    pub lat: f64,
    #[serde(default)]
    pub note: String,
    pub created: u64,
}

/// Zapisany widok: kamera i zestawienie warstw.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SavedView {
    pub id: u64,
    pub name: String,
    /// (dł., szer., wysokość, azymut, nachylenie, odległość) jak w `Prefs::camera`.
    pub camera: (f64, f64, f64, f64, f64, f64),
    pub style: Style,
    #[serde(default = "one")]
    pub exag: f64,
    /// Włączone warstwy z katalogu, od spodu stosu.
    #[serde(default)]
    pub layers: Vec<LayerState>,
    /// Włączone dane na żywo.
    #[serde(default)]
    pub feeds: Vec<String>,
    pub created: u64,
}

fn one() -> f64 {
    1.0
}

impl SavedView {
    /// Krótki opis: styl i liczba warstw.
    pub fn summary(&self) -> String {
        let n = self.layers.len() + self.feeds.len();
        let warstwy = match n {
            0 => "bez warstw".to_string(),
            1 => "1 warstwa".to_string(),
            2..=4 => format!("{n} warstwy"),
            _ => format!("{n} warstw"),
        };
        format!("{} · {warstwy} · {}", self.style.label(), date_label(self.created))
    }
}

#[derive(Default, Serialize, Deserialize)]
pub struct Library {
    #[serde(default)]
    pub places: Vec<Place>,
    #[serde(default)]
    pub views: Vec<SavedView>,
    #[serde(default)]
    next_id: u64,
}

fn file() -> PathBuf {
    crate::settings::config_dir().join("biblioteka.json")
}

/// Bieżący czas (sekundy od 1970).
pub fn now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// Data zapisu po polsku: „28.11.2026”.
pub fn date_label(secs: u64) -> String {
    let local = secs as f64 + crate::sun::warsaw_offset_min(secs as f64) as f64 * 60.0;
    let d = crate::sun::civil_from_days((local / 86_400.0).floor() as i64);
    format!("{}.{:02}.{}", d.d, d.m, d.y)
}

impl Library {
    pub fn load() -> Self {
        std::fs::read(file()).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
    }

    pub fn save(&self) -> std::io::Result<()> {
        std::fs::create_dir_all(crate::settings::config_dir())?;
        let tmp = file().with_extension("tmp");
        std::fs::write(&tmp, serde_json::to_vec_pretty(self).unwrap_or_default())?;
        std::fs::rename(tmp, file())
    }

    pub fn next_id(&mut self) -> u64 {
        let used = self.places.iter().map(|p| p.id).chain(self.views.iter().map(|v| v.id)).max().unwrap_or(0);
        self.next_id = self.next_id.max(used) + 1;
        self.next_id
    }

    /// Nazwa bez powtórzeń: „Widok”, „Widok 2”, „Widok 3”…
    pub fn unique_name(&self, base: &str) -> String {
        let taken = |n: &str| self.views.iter().any(|v| v.name == n) || self.places.iter().any(|p| p.name == n);
        if !taken(base) {
            return base.to_string();
        }
        (2..).map(|i| format!("{base} {i}")).find(|n| !taken(n)).unwrap_or_else(|| base.to_string())
    }

    pub fn rename(&mut self, id: u64, name: &str) {
        let name = name.trim();
        if name.is_empty() {
            return;
        }
        if let Some(p) = self.places.iter_mut().find(|p| p.id == id) {
            p.name = name.to_string();
        }
        if let Some(v) = self.views.iter_mut().find(|v| v.id == id) {
            v.name = name.to_string();
        }
    }

    pub fn remove(&mut self, id: u64) {
        self.places.retain(|p| p.id != id);
        self.views.retain(|v| v.id != id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_and_ids() {
        let mut lib = Library::default();
        let a = lib.next_id();
        let b = lib.next_id();
        assert!(b > a);
        lib.places.push(Place { id: a, name: "Widok".into(), lon: 21.0, lat: 52.2, note: String::new(), created: 0 });
        assert_eq!(lib.unique_name("Widok"), "Widok 2");
        lib.rename(a, "  Dom ");
        assert_eq!(lib.places[0].name, "Dom");
        lib.remove(a);
        assert!(lib.places.is_empty());
        // stary plik bez nowych pól też się wczytuje
        let old: Library = serde_json::from_str(r#"{"places":[{"id":1,"name":"X","lon":20.0,"lat":50.0,"created":0}]}"#).unwrap();
        assert_eq!(old.places.len(), 1);
    }

    #[test]
    fn date_is_polish() {
        // 1 stycznia 2026, południe UTC
        assert_eq!(date_label(1_767_268_800), "1.01.2026");
    }
}
