# Polskie Studio Map – zasady pracy w repozytorium

Plan przebudowy: `docs/PLAN-MULTIMAPA.md`.

- **Każda zmiana trafia do `main`**: praca na gałęzi roboczej → pull request do `main` → scalenie (merge). Nie zostawiamy zmian tylko na gałęzi.
- **Tylko Windows, tylko Rust.** Program natywny (Rust + wgpu + egui) w katalogu głównym (`Cargo.toml`, `src/`). Zasięg: wyłącznie Polska.
- **Nie budujemy i nie testujemy lokalnie** – kompilację i testy robi GitHub Actions; błędy poprawiamy na podstawie logów przebiegu.
- **Wydania** (instalator NSIS `PSM-Setup-<wersja>.exe` + `latest.yml` w GitHub Releases) robi workflow `.github/workflows/build.yml`:
  push na `main` z nową wersją w `Cargo.toml` (brak tagu `v<wersja>`) → testy, build, podpis, tag i wydanie.
  Bez podbicia wersji push na `main` to tylko kontrolna kompilacja.
- Przy wydaniu podbij `version` w `Cargo.toml` (i `Cargo.lock`) i dodaj opis `build/release-notes/<wersja>.md`.
- **Pobieranie i aktualizacje idą przez redzikowski.com** (projekt `polskie-studio-map` w portfolio strony, repozytorium `BlackSingularities/Homepage`, `releaseSource: 'github'`). Program pyta `redzikowski.com/api/github/repos/BlackSingularities/polskie-studio-map/releases/latest`, pobiera instalator przez serwer strony, sprawdza SHA-512 z `latest.yml` i uruchamia go z UAC (`src/updater.rs`). Wydanie bez `latest.yml` zablokuje aktualizacje.
- Instalator NSIS (`build/psm.nsi`, UTF-8 z BOM) instaluje dla wszystkich użytkowników (Program Files, administrator); tryb cichy `/S`, `--force-run` uruchamia program po instalacji.
- Dane map: jeden folder (`<wybrany>\PSM-dane`) z limitem 1–500 GB, zapisywane samoczynnie podczas oglądania; po przekroczeniu limitu usuwane od najdawniej używanych: paczki kafli `tiles\`, sektory OSM `sectors\`. Ustawienia w `%APPDATA%\PSM\psm.json`, ścieżka danych też w `datadir.txt` (dla deinstalatora).
- Struktura: `render/` (kamera, kafle, shadery WGSL), `ui/atlas.rs` (widok 3D, narzędzia), `ui/panels.rs`, `trails.rs` (sieć szlaków), `osm.rs` + `world.rs` (sektory OSM), `national.rs` (zasięg Polski, ortofoto), `heights.rs`, `analysis.rs`, `store.rs` + `pack.rs` (dane na dysku).
