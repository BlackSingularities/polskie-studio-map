# Polskie Studio Map — atlas warstw mapowych Polski 3D

Natywny program dla Windows (Rust, wgpu, egui): trójwymiarowy atlas Polski łączący rzeczywisty model terenu, ortofotomapę i dane OpenStreetMap z narzędziami analitycznymi — wysokością względną, polem widoczności, linią wzroku i cieniami o dowolnej porze dnia. Program powstaje jako multimapa z dziesiątkami warstw z oficjalnych źródeł; plan prac: [docs/PLAN-MULTIMAPA.md](docs/PLAN-MULTIMAPA.md).

Jeden plik `psm.exe`, bez przeglądarki, Node i lokalnego serwera. Dystrybucja przez [redzikowski.com](https://redzikowski.com/portfolio/polskie-studio-map) (pliki z GitHub Releases tego repozytorium). Repozytorium nie zawiera danych map: pobierane są i zapisywane podczas oglądania.

## Instalacja

| platforma | plik | uwagi |
| --- | --- | --- |
| Windows 10/11 (64-bit) | `PSM-Setup-<wersja>.exe` | instalator dla wszystkich użytkowników (`Program Files`, okno UAC, wybór folderu), skróty, deinstalacja z Panelu sterowania, aktualizacje z redzikowski.com |

## Funkcje (0.1.0)

- **Zasięg: Polska.** Kamera nie wychodzi poza kraj (zapas ~10 km), a kafle spoza niego nie są pobierane; ortofoto, nazwy i szlaki przycinane są do granicy.
- **Teren** kaflowy z poziomami szczegółów; style: teren, satelita (Sentinel-2 cloudless + ortofoto GUGiK), mapa, hipsometria, nachylenie, ekspozycja.
- **Słońce według daty i godziny**, cienie rzeźby, śnieg i barwy pory roku, poziomice, siatka kilometrowa, przewyższenie ×1–3.
- **Szlaki i punkty z OSM**, pokrycie terenu i wyszukiwarka miejsc w Polsce (Nominatim).
- **Analizy:** soczewki (`H` `W` `O`), pole widoczności, pomiar po terenie, linia wzroku, panorama.
- Linki `psm://` do widoku, zrzut ekranu (`S`), skróty klawiszowe (`?`).

## Dane na dysku

```text
<folder>\PSM-dane\
  tiles\dem\z\bx_by.gpk         teren (u16 co 1/8 m + LZ4), paczki 16×16 kafli
  tiles\sat\z\bx_by.gpk         zdjęcia satelitarne (JPEG), paczki 16×16 kafli
  tiles\ortho\z\bx_by.gpk       ortofotomapa (JPEG 512 px)
  sectors\9_x_y.json.lz4        szlaki i punkty OSM sektora
  land\9_x_y.png                pokrycie terenu sektora
```

Po przekroczeniu limitu (1–500 GB) najdawniej używane dane są usuwane. Ustawienia: `%APPDATA%\PSM\psm.json` (i `datadir.txt` dla deinstalatora).

Źródła: teren **Mapterhorn** (NMT GUGiK), zapasowo **Terrarium** (AWS Open Data), ortofoto **GUGiK**, zdjęcia **Sentinel-2 cloudless 2020 © EOX** (CC BY-NC-SA 4.0), szlaki i punkty **© OpenStreetMap** (ODbL), granica kraju **Natural Earth**.

## Budowanie i wydania

Kompilację i testy wykonuje GitHub Actions. Wydanie: podbij `version` w `Cargo.toml` i `Cargo.lock`, dodaj `build/release-notes/<wersja>.md` i scal zmiany do `main` – workflow `.github/workflows/build.yml` uruchomi testy, zbuduje program i instalator (`build/psm.nsi`), utworzy tag `v<wersja>` i opublikuje wydanie z `latest.yml`. Podpis cyfrowy (opcjonalne sekrety): `WIN_CSC_LINK` i `WIN_CSC_KEY_PASSWORD`.

Zmienne do testów: `PSM_CAPTURE=plik.png` i `PSM_CAPTURE_AFTER=sekundy`.

---

**Projekt i wykonanie: [redzikowski.com](https://redzikowski.com)**

<a href="https://buycoffee.to/adamredzikowski" target="_blank"><img src="https://buycoffee.to/static/img/share/share-button-dark--pl.png" style="width: 351px; height: 92px" width="351" height="92" alt="Postaw kawę dla adamredzikowski na buycoffee.to"></a>
