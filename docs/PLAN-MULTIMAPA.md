# Polskie Studio Map — plan przebudowy GRANI w multimapę Polski

> Dokument roboczy dla kolejnych sesji. Stan: 2026-11-04. Repozytorium powstało jako kopia `gran-3d` (historia zachowana, `origin` usunięty — nowe repo na GitHubie trzeba dopiero założyć).
> Zasady pracy z `CLAUDE.md` obowiązują dalej (Rust + wgpu + egui, tylko Windows, build i testy wyłącznie w GitHub Actions, PR do `main`).

## 0. Cel w jednym akapicie

Z „wyświetlarki map do chodzenia po górach” robimy **Polskie Studio Map**: natywny atlas 3D całej Polski (teren + budynki + ortofoto), na którym użytkownik **włącza i wyłącza dziesiątki–setki warstw** z oficjalnych źródeł (GUGiK, GDOŚ, IMGW, GUS, PIG-PIB, PSE, GIOŚ, KZGW/Wody Polskie, ISOK, NID, PIG, BDL…) oraz z otwartych projektów (OSM, Copernicus/Sentinel, Open-Meteo, Blitzortung…). Zasięg **wyłącznie Polska** — wszystko, co spoza PL (SK/CZ ortofoto, Mapterhorn dla świata, Terrarium, Natural Earth innych krajów), wylatuje albo jest przycinane do granicy PL. Wyszukiwanie tras (`graph.rs`, profil trasy, GPX-eksport tras) usuwamy na razie w całości.

---

## 1. Co zostaje, co wylatuje, co się zmienia (obecny kod ≈ 12,4 tys. linii)

| Moduł | Decyzja | Uwagi |
|---|---|---|
| `render/` (kamera, kafle, WGSL) | **zostaje, rozbudowa** | dodać kompozycję wielu warstw rastrowych (stos z przezroczystością) i warstw wektorowych drapowanych na teren |
| `heights.rs`, `download.rs` (Mapterhorn) | **zmiana** | podstawowo NMT GUGiK (1 m / ~5 m) dla PL; Mapterhorn tylko jako zapas i tylko w bbox PL |
| `store.rs`, `pack.rs`, `bake.rs` | **zostaje** | rozszerzyć klucz pamięci podręcznej o `layer_id` + wersję warstwy; polityka LRU per warstwa |
| `national.rs` | **zmiana** | tylko PL; zostaje jako zalążek ogólnego klienta WMS/WMTS |
| `assets/coverage.json` | **zmiana** | tylko granica PL (PRG, uproszczona) + maska do przycinania |
| `osm.rs`, `world.rs`, `mvt.rs` | **zostaje** | OSM jako jedna z grup warstw; Overpass zapytania per warstwa tematyczna |
| `landcover.rs` | **zostaje** | → warstwa „Pokrycie terenu” (CLC / ESA WorldCover / BDOT10k) |
| `sun.rs` | **zostaje** | → warstwa „Nasłonecznienie / cienie” |
| `analysis.rs`, `metrics.rs` | **przegląd** | zostawić narzędzia pomiarowe (odległość, pole, przekrój terenu), usunąć to, co dotyczy tras |
| `graph.rs`, `ui/profile.rs`, część `gpx.rs` | **usunąć** | wyszukiwanie tras — do zastąpienia później (np. „izochrony”/„dojazd” z GTFS) |
| `ui/search.rs` | **zmiana** | Nominatim → dodatkowo PRG/UUG GUGiK (adresy, działki — `uldk.gugik.gov.pl`) |
| `ui/panels.rs`, `ui/atlas.rs` | **duża zmiana** | nowy panel „Warstwy” (drzewo, wyszukiwarka, przezroczystość, legenda, kolejność) |
| `updater.rs` | **zmiana** | nowy FEED (`…/repos/BlackSingularities/polskie-studio-map/…`), nowa nazwa instalatora, nowe `%APPDATA%\PSM` |
| `build/gran.nsi`, `build.yml` | **zmiana** | nazwa produktu, GUID, instalator `PSM-Setup-<wersja>.exe`, wersja startowa 0.1.0 |

---

## 2. Architektura warstw (rdzeń całej przebudowy)

### 2.1. Model danych
```rust
pub struct LayerDef {
    id: &'static str,            // "gdos.natura2000.oso"
    group: Group,                // Geo, Przyroda, Meteo, Woda, Statystyka, Urban, Transport, Energia, Historia, …
    name: &'static str,          // po polsku
    kind: LayerKind,             // RasterXYZ | Wmts | Wms | WmsTimeSeries | VectorMvt | GeoJson | Wfs | PointFeed | Grid | Terrain | Model3d
    source: SourceDef,           // URL szablon, warstwa WMS, styl, CRS (EPSG:2180 / 3857 / 4326), format
    zoom: RangeInclusive<u8>,
    refresh: Refresh,            // Static | Daily | Hourly | Minutes(n) | Live
    attribution: &'static str,   // wymagane licencją
    licence: Licence,
    legend: Legend,              // auto z WMS GetLegendGraphic albo własna rampa
    default_opacity: f32,
    blend: Blend,                // Normal | Multiply (cieniowanie) | Screen
    query: Option<QueryDef>,     // GetFeatureInfo / atrybuty → panel szczegółów po kliknięciu
}
```
- **Katalog warstw jako dane**: `assets/layers/*.toml` (wbudowane `include_str!`) + możliwość dociągania aktualizacji katalogu z redzikowski.com bez nowego wydania. Dzięki temu dodanie warstwy = dopisanie wpisu, nie kod.
- Rodzaje silnika (każdy jako `trait LayerSource`): `XyzRaster`, `WmtsRaster` (GUGiK ma WMTS w EPSG:2180 i 3857), `WmsRaster` (kafelkowanie „na żądanie” bbox 256/512 px, jak dziś w `national.rs`), `WmsTime` (parametr `TIME` — radar, archiwalne ortofoto), `VectorTiles` (MVT — już mamy `mvt.rs`), `GeoJsonFeed` (punkty/poligony, np. stacje IMGW), `GridField` (siatka wartości → kolorowanie w shaderze, np. temperatura Open-Meteo), `TerrainSource` (NMT), `Buildings3d` (BDOT10k/OSM + LOD1/LOD2 GUGiK).

### 2.2. Render
- Stos warstw rastrowych składany **na GPU** jako tablica tekstur na kafel terenu (texture array, max ~8 aktywnych warstw rastrowych naraz, reszta kompozytowana wcześniej do jednej tekstury CPU-side). Przezroczystość i tryb mieszania per warstwa w uniformie.
- Wektory (linie/punkty/poligony) — drapowane na teren (`lines.wgsl` już jest), etykiety w egui w przestrzeni ekranu z odrzucaniem kolizji.
- Pola ciągłe (meteo, smog, statystyka gmin) — tekstura wartości + rampa kolorów w shaderze → zmiana palety bez ponownego pobierania; suwak czasu.
- Przełącznik **2D/3D** i „przesada wysokości” (już jest) — w 2D warstwy tematyczne czytelniejsze.

### 2.3. UI
- Panel **Warstwy**: drzewo grup → warstwy, checkbox, suwak przezroczystości, ↑↓ kolejność, ikona „i” (opis, źródło, licencja, data aktualizacji), legenda zwijana.
- **Wyszukiwarka warstw** (po nazwie/tagu — przy 200+ warstwach obowiązkowa).
- **Zestawy (presety)**: „Turystyka”, „Przyroda”, „Pogoda teraz”, „Powódź”, „Nieruchomości/działka”, „Energia”, „Historia”, „Smog”. Zapis własnych zestawów w `%APPDATA%\PSM\presets.json`.
- **Oś czasu** dla warstw czasowych (radar, prognozy, archiwalne ortofoto od lat 50., zmiany pokrycia terenu).
- **Kliknięcie w mapę** → zapytanie do wszystkich aktywnych warstw z `query` (GetFeatureInfo/WFS/atrybuty) → zbiorczy panel szczegółów (już mamy panel szczegółów 2.3.0).
- **Porównywarka (swipe)**: dwie warstwy z suwakiem (np. ortofoto 2024 vs 1965).
- Status per warstwa: ładuje się / błąd serwisu / poza zakresem powiększeń.

### 2.4. Ograniczenie do Polski
- Bbox PL w EPSG:4326 ≈ lon 14.07–24.15, lat 49.00–54.84; kamera i pobieranie nie wychodzą poza bbox + 10 km.
- Maska granicy (PRG, uproszczona do ~2 MB) → poza Polską teren wygaszony/szary, kafle spoza nie są pobierane.
- Siatki statystyczne/meteo przycinane maską.

---

## 3. Katalog źródeł danych (kandydaci na warstwy)

> Legenda: **[W]** WMS/WMTS raster, **[F]** WFS/wektor, **[A]** REST/JSON API, **[P]** plik do pobrania/przetworzenia (bake do paczek), **[L]** dane „na żywo”.
> Adresy z pamięci — **przed implementacją każdej warstwy zweryfikować GetCapabilities** i licencję. Większość danych GUGiK jest od 2020 r. bezpłatna (ustawa o informatyzacji, art. 40a PGiK) — z obowiązkiem podania źródła.

### 3.1. Geodezja i kartografia — GUGiK / Geoportal (`mapy.geoportal.gov.pl`, `integracja.gugik.gov.pl`)
- **Ortofotomapa** HighResolution (jest), Standard, **archiwalne ortofoto i zdjęcia lotnicze** (od lat 50.) [W, czasowe] — `…/PZGIK/ORTO/WMS/…`, WMTS `…/WMTS/StandardResolution`
- **Ortofoto „prawdziwe” (True Ortho)** dla miast [W]
- **NMT** (numeryczny model terenu, 1 m z LIDAR) i **NMPT** (model pokrycia — drzewa, budynki) [W cieniowanie + P GeoTIFF/ASC z `opendata.geoportal.gov.pl`] → nasz teren 3D o rozdzielczości dużo lepszej niż Mapterhorn
- **Cieniowanie rzeźby** NMT [W] — gotowa warstwa „Hillshade”
- **Chmury punktów LIDAR** (ISOK, LAZ) [P] — opcjonalnie, do 3D wysokiej szczegółowości
- **Modele 3D budynków LOD1 / LOD2** (CityGML) [P] — cała Polska; przerobić w `bake.rs` na nasze siatki
- **BDOT10k** (baza danych obiektów topograficznych: budynki, drogi, rzeki, lasy, granice, obiekty) [W + F + P GML per powiat]
- **BDOO** (ogólnogeograficzna, mniejsze skale) [W]
- **Mapa topograficzna** rastrowa (1:10k, archiwalne WIG 1:100k z lat 30., mapy powojenne) [W]
- **PRG — granice administracyjne** (województwa, powiaty, gminy, obręby) [W + F + P SHP] — rdzeń dla warstw statystycznych
- **PRNG — nazwy geograficzne** [F] — etykiety po polsku
- **PRPOG / EMUiA — adresy** [W + F]; **Krajowa Integracja Ewidencji Gruntów (KIEG) — działki** [W] (`integracja.gugik.gov.pl/cgi-bin/KrajowaIntegracjaEwidencjiGruntow`)
- **ULDK API — wyszukiwanie działki po numerze/współrzędnych** [A] `uldk.gugik.gov.pl`
- **Krajowa Integracja Uzbrojenia Terenu (KIUT)** — sieci: wodociąg, gaz, prąd, telekom [W]
- **Krajowa Integracja MPZP / plany miejscowe** (`integracja.gugik.gov.pl/cgi-bin/KrajowaIntegracjaMiejscowychPlanowZagospodarowaniaPrzestrzennego`) [W] + **Rejestr urbanistyczny / plany ogólne** (od 2026 obowiązkowe) [W]
- **Osnowa geodezyjna** (punkty) [W]
- **Siatki kartograficzne** (godła map, siatka 1992) [W]
- **Ceny transakcyjne nieruchomości (RCN)** — od 2026 bezpłatnie [W/F] — mega warstwa „rynek nieruchomości”
- **Usługa wysokości punktu** (`services.gugik.gov.pl/nmt/?request=GetHbyXY`) [A] — dokładna wysokość w miejscu kliknięcia

### 3.2. Przyroda i środowisko
- **GDOŚ** (`sdi.gdos.gov.pl/wms`): **parki narodowe, krajobrazowe, rezerwaty, Natura 2000 (OSO/SOO), obszary chronionego krajobrazu, pomniki przyrody, użytki ekologiczne, zespoły przyrodniczo-krajobrazowe, korytarze ekologiczne** [W + F]
- **Bank Danych o Lasach (BDL)** (`bdl.lasy.gov.pl`, WMS/WFS) — wydzielenia leśne, gatunki drzew, wiek drzewostanu, nadleśnictwa, **strefy „zanocuj w lesie”**, rezerwaty leśne, ostoje [W + F]
- **Lasy Państwowe — zagrożenie pożarowe lasów** (dzienne, IBL) [W/L]
- **GIOŚ**: **jakość powietrza API** (`api.gios.gov.pl/pjp-api/v1/rest/…` — stacje, indeks, pomiary PM10/PM2.5/NO₂/O₃) [A, L]; **monitoring hałasu, PEM**, mapy akustyczne miast [W]
- **Airly / Syngeos / Looko2 / sensor.community** — gęsta sieć czujników smogu [A, L] (sensor.community: `data.sensor.community/static/v2/data.json` — przyciąć do PL)
- **CAMS / Copernicus Atmosphere** — prognoza smogu w siatce (przez Open-Meteo Air Quality API) [A]
- **Promieniowanie** — PAA (Państwowa Agencja Atomistyki) stacje PMS [A/L]; **radon** (mapy PIG) [W]
- **Corine Land Cover** (2018, zmiany) [W/P]; **ESA WorldCover 10 m** [P]; **Copernicus HRL** (lasy, nieprzepuszczalność, łąki, wilgotność) [P]
- **Sentinel-2 / Landsat** — kompozycje, NDVI, susza (Copernicus Data Space / Sentinel Hub EO Browser WMS; EOX s2cloudless już używamy) [W]
- **Susza rolnicza — IUNG** (`susza.iung.pulawy.pl`) [W]; **susza hydrologiczna — IMGW/Stop Suszy** [W]
- **Gleby** — mapa glebowo-rolnicza IUNG, klasy bonitacyjne (z EGiB) [W]
- **Siedliska, gatunki** — GBIF occurrence API (`api.gbif.org/v1/occurrence/search?country=PL`) [A]; **iNaturalist** [A]; **eBird / ornitho.pl** [A]
- **Drzewa pomnikowe, aleje** — CRFOP (GDOŚ) [F]
- **Mapa zanieczyszczeń / PRTR** — zakłady emitujące [F]

### 3.3. Woda i zagrożenia — Wody Polskie, KZGW, ISOK, IMGW
- **Mapy zagrożenia i ryzyka powodziowego (ISOK)** — Q1%, Q10%, Q0,2%, scenariusz zniszczenia wału (`wody.isok.gov.pl` / Hydroportal) [W]
- **MPHP — mapa podziału hydrograficznego** (rzeki, zlewnie, JCWP) [W + F]
- **Hydroportal KZGW** (`wody.isok.gov.pl/gpservices/…`) — wody podziemne GZWP, JCWPd, ujęcia, strefy ochronne [W]
- **IMGW hydro API** (`danepubliczne.imgw.pl/api/data/hydro`) — **stany wód na wodowskazach na żywo**, ostrzeżenia hydrologiczne [A, L]
- **Kąpieliska — serwis kąpieliskowy GIS** (sanepid; jakość wody, sinice) [A/F]
- **Jeziora — batymetria** (IRŚ/IMGW, rastrowe plany) [W/P]; obecna „woda w jeziorach” z 2.3.0 do zachowania
- **Wały przeciwpowodziowe, zbiorniki, jazy, śluzy** [F]
- **Osuwiska — SOPO (PIG-PIB)** [W + F] — świetnie wygląda w 3D w Karpatach
- **Zagrożenie lawinowe — TOPR/GOPR** (komunikaty) [A/L]
- **Zalew sztormowy / erozja brzegu** — Urząd Morski, ISOK strefa przybrzeżna [W]

### 3.4. Meteo i klimat
- **IMGW-PIB danepubliczne** (`danepubliczne.imgw.pl/api/data/synop`, `/meteo`, `/warningsmeteo`, `/warningshydro`) — stacje synop na żywo, **ostrzeżenia meteorologiczne per powiat** [A, L]; archiwa dobowe/miesięczne od 1951 (pliki CSV) [P] → normy klimatyczne, anomalie
- **Radar opadów IMGW** (`danepubliczne.imgw.pl` dane radarowe HDF5 / produkty CMAX, PAC) [P/L] — animacja ostatnich 2–3 h
- **Burze / wyładowania** — Blitzortung (WebSocket, licencja niekomercyjna!) [L]; PERUN IMGW (jeśli dostępny)
- **Open-Meteo** (`api.open-meteo.com` — prognoza ICON-D2/ECMWF/GFS w siatce, `air-quality-api`, `archive-api` ERA5, `flood-api` GloFAS, `marine-api` Bałtyk) [A] — **główne źródło pól prognozy** (temp., opad, wiatr, chmury, śnieg, UV, punkt rosy) z suwakiem godzin; bezpłatnie do użytku niekomercyjnego
- **ICON-D2 / COSMO-PL / UM-ICM** — ICM UW `meteo.pl` (UM 4 km, mapy) [W/P] — polski model
- **Satelita** — EUMETSAT (MTG/Meteosat WMS `view.eumetsat.int/geoserver/wms`) — chmury, IR, burze na żywo [W, L]
- **Pokrywa śnieżna** — IMGW + Copernicus Snow / H SAF [W]
- **Wiatr jako animowane cząstki** (jak earth.nullschool) — z pól Open-Meteo, shader cząstek
- **Klimat** — Klimada 2.0 (scenariusze zmian klimatu dla PL) [W/P]; **fenologia, pyłki** — Open-Meteo pollen (CAMS) [A]
- **Zorza / Kp** — NOAA SWPC (to globalne, ale tylko jako wskaźnik dla PL) [A]

### 3.5. Geologia i zasoby — PIG-PIB (`geologia.pgi.gov.pl`, CBDG)
- **Szczegółowa Mapa Geologiczna Polski 1:50 000 (SMGP)** [W]; mapa geologiczna 1:500k [W]
- **Złoża kopalin (MIDAS)**, obszary i tereny górnicze, koncesje [W + F]
- **Otwory wiertnicze (CBDG)** [F]; **geotermia** [W]; **geozagrożenia, osuwiska (SOPO)**, **tereny pogórnicze/szkody górnicze**
- **Mapa hydrogeologiczna (MhP)**, GZWP [W]
- **Sejsmika / wstrząsy górnicze** — EMSC/GFZ przycięte do PL, LGOM/GZW [A, L]
- **Geostanowiska i geoparki** (CRG) [F]

### 3.6. Statystyka — GUS i inne
- **BDL GUS API** (`bdl.stat.gov.pl/api/v1/`) — tysiące wskaźników per gmina/powiat/województwo (ludność, wiek, bezrobocie, dochody gmin, mieszkania, edukacja, zdrowie, turystyka, środowisko) → **kartogramy** sklejone z PRG; to samo API = setki warstw z jednego silnika (wybór zmiennej + rok w UI)
- **Siatka 1 km GUS (NSP 2021)** — ludność w kwadratach 1 km / 250 m (`geo.stat.gov.pl`) [W/P] — gęstość zaludnienia w 3D jako słupki!
- **Geostatystyczny portal GUS** (`geo.stat.gov.pl`) — wskaźniki NSP [W]
- **TERYT API** (SOAP, `api.stat.gov.pl` / eTERYT) — słowniki jednostek, miejscowości [A]
- **REGON / rejestr podmiotów** (agregaty wg gmin) [A]; **CEIDG** [A]
- **Wybory — PKW** (wyniki per obwód/gmina, `wybory.gov.pl` / `danewyborcze.kbw.gov.pl`) [P] — kartogramy wyborcze
- **Ceny nieruchomości — RCN** (patrz 3.1), **NBP** (raporty cen mieszkań) [P]
- **Dane.gov.pl API** (`api.dane.gov.pl`) — **meta-źródło**: przeszukiwalny katalog tysięcy zbiorów; warto zbudować „importer” warstw z dane.gov.pl (CSV/GeoJSON z kolumnami współrzędnych) jako narzędzie w aplikacji
- **Eurostat NUTS / GISCO** (tylko PL) — porównania regionów [A]
- **Rejestr szkół RSPO API** (`api-rspo.men.gov.pl`) [A]; **wyniki egzaminów CKE** [P]
- **Apteki, przychodnie, szpitale** — Rejestr Aptek, NFZ API (`api.nfz.gov.pl` — kolejki do lekarzy!, placówki) [A]
- **Wypadki drogowe — SEWiK (policja)** [P/A]; **mapa zagrożeń bezpieczeństwa (KMZB)** [W/A]
- **Przestępczość — statystyki KGP** per powiat [P]

### 3.7. Urban, budynki, infrastruktura
- **Budynki 3D** — GUGiK LOD1/LOD2 + BDOT10k + OSM (`building:levels`, już mamy piętra wg szerokości) [P]
- **EGiB — budynki i działki** (KIEG) [W]
- **Plany miejscowe / plany ogólne / decyzje WZ** [W] (patrz 3.1)
- **Rejestr cen (RCN)**, **Geoportale miejskie** (Warszawa, Kraków, Wrocław, Poznań, Gdańsk, Łódź — własne WMS: drzewa, hałas, termowizja, zieleń, ścieżki) [W] — dodać jako grupa „Miasta”
- **Termowizja budynków** (Kraków, Warszawa, Łódź — lotnicze mapy termalne) [W]
- **Drzewostan miejski / inwentaryzacja drzew** (Warszawa, Wrocław) [F]
- **Zabytki — NID** (`mapy.zabytek.gov.pl/nid/` WMS/WFS: rejestr zabytków nieruchomych, archeologiczne AZP, pomniki historii, UNESCO) [W + F]
- **Mapy historyczne** — archiwalne WIG, Messtischblätter (via `mapy.amzp.pl`, `polona.pl`), mapa Heinricha/Gilly’ego (Geoportal archiwalny), mapy katastralne — warstwy czasowe z porównywarką
- **Oświetlenie nocne** — VIIRS Black Marble (NASA GIBS WMTS) przycięte do PL [W]
- **Zielona/niebieska infrastruktura, wyspy ciepła** — Landsat LST (Copernicus/USGS) [P]
- **Dostępność usług (15-minutowe miasto)** — liczone lokalnie z OSM + GTFS (izochrony — przyszły zamiennik modułu tras)

### 3.8. Transport i ruch
- **GTFS** — przewoźnicy kolejowi (PKP Intercity, Polregio, KM, SKM, KD…), MPK/ZTM większych miast (GitHub: `MKuranowski/PolishTrainsGTFS`, `mkuran.pl/gtfs/`; `dane.gov.pl`) [P] — przystanki, linie, częstotliwości
- **Pozycje pojazdów na żywo** — ZTM Warszawa API (`api.um.warszawa.pl`), GTFS-RT Kraków/Gdańsk/Poznań/Wrocław [A, L]
- **PKP PLK — linie kolejowe, stacje, maksymalne prędkości** (OpenRailwayMap jako kafle `tiles.openrailwaymap.org` — tylko PL) [W]
- **GDDKiA** — **utrudnienia, remonty, natężenie ruchu (GPR)**, sieć dróg krajowych, MOP-y, ważenie (`archiwum.gddkia.gov.pl`, `dane.gov.pl`) [A/P]
- **Lotnictwo** — PAŻP: strefy przestrzeni powietrznej, **DroneMap / strefy dla dronów** (`drony.gov.pl`, AUP) [W/A]; **ruch samolotów** OpenSky Network API (bbox PL) [A, L]; lotniska, lądowiska ULC
- **Żegluga** — AIS (Bałtyk, Zalew, Odra — `aisstream.io` WebSocket) [L]; szlaki wodne, śluzy (Wody Polskie, RIS Odra)
- **Rowery** — sieć tras (OSM `route=bicycle`, EuroVelo, Velo Małopolska), stacje rowerów miejskich (GBFS — Nextbike PL, `gbfs.nextbike.net`) [A, L]
- **Ładowarki EV** — EIPA UDT API (`eipa.udt.gov.pl`) — **oficjalny rejestr wszystkich ładowarek z cenami i dostępnością** [A, L]; stacje paliw + ceny (e-petrol nieoficjalnie)
- **Fotoradary, odcinkowe pomiary** — CANARD GITD [F]
- **Ruch na drogach (natężenie)** — GPR GDDKiA [P]; prędkości z OSM `maxspeed`

### 3.9. Energia i telekomunikacja
- **PSE — linie i stacje NN** (OSM `power=*` + PSE mapa sieci), **generacja na żywo, ceny, rezerwy, przepływy transgraniczne** (`api.raporty.pse.pl` — API raportów PSE v2) [A, L]
- **Farmy wiatrowe i PV** — OSM `generator:source`, URE rejestry koncesji / mikroinstalacje per gmina (URE, dane.gov.pl) [F/P]; **Global Wind Atlas / Global Solar Atlas** (przycięte) [W]
- **Potencjał OZE na dachach** — z NMPT + LOD2 + `sun.rs` liczone lokalnie (nasza unikalna funkcja!)
- **Gazociągi, rurociągi** — Gaz-System (mapa), OSM `man_made=pipeline` [F]
- **Zasięgi i BTS-y** — UKE: **mapa nadajników (SI2PEM / `si2pem.gov.pl`)**, `btsearch.pl`, **zasięgi internetu stacjonarnego (UKE „Inwentaryzacja”)**, „białe plamy” [A/W]
- **Szybki internet — mapa SIDUSIS** (`internet.gov.pl`) [W]

### 3.10. Turystyka, rekreacja, kultura (spuścizna GRANI)
- **Szlaki turystyczne** PTTK (OSM `route=hiking` z kolorami — już mamy), szlaki narciarskie, kajakowe, konne
- **Schroniska, baza noclegowa** (OSM + e-Turysta/Centralny Wykaz Obiektów Hotelarskich — `dane.gov.pl`) [F]
- **Parki narodowe — limity wejść, warunki** (TPN API? — do sprawdzenia) [A]
- **Kamery** — TOPR, TPN, webcamy górskie, kamery drogowe GDDKiA (punkty z linkiem do obrazu) [A]
- **Grzyby, jagody, łowiska** — PZW łowiska [F], obwody łowieckie (BDL / `mapa.lasy.gov.pl`) [W]
- **Pola biwakowe LP, miejsca postojowe LP** (BDL) [F]
- **Wikipedia/Wikidata geo** — artykuły w okolicy (`pl.wikipedia.org/w/api.php?list=geosearch`), Wikidata SPARQL (zabytki, zdjęcia Wikimedia Commons z geotagami) [A]
- **Kościoły, cmentarze, miejsca pamięci** (IPN — groby wojenne, „Miejsca Pamięci Narodowej”) [F]
- **Muzea, biblioteki, teatry** — OSM + rejestry MKiDN [F]

### 3.11. Rolnictwo
- **ARiMR — LPIS / działki referencyjne, wnioski (uprawy deklarowane)** [W] (`geoportal.arimr.gov.pl`)
- **ASF / ptasia grypa — strefy GIW** [W/F] — aktualne strefy chorób zwierząt
- **Obszary ONW, azotanowe** [W]
- **Plony, susza, pogoda agro** — IUNG, Open-Meteo (evapotranspiration, soil moisture) [A]

### 3.12. Bezpieczeństwo i państwo
- **Strefy przygraniczne, zakazy wstępu (np. strefa przy granicy z BY)** [F/A]
- **Ostrzeżenia RCB** (`rcb.gov.pl` alerty) [A, L]
- **Jednostki PSP, OSP, policja, SOR-y, LPR** [F]; **defibrylatory AED** (OSM `emergency=defibrillator`) [F]
- **Sądy, urzędy, punkty nieodpłatnej pomocy prawnej** (`np.ms.gov.pl` API) [A]

### 3.13. Repozytoria GitHub / projekty otwarte warte przejrzenia
- **`MKuranowski/PolishTrainsGTFS`**, `MKuranowski/WarsawGTFS` — gotowe GTFS kolei i ZTM
- **`kodujdlapolski/*`** (Koduj dla Polski — np. powietrze, dane wyborcze)
- **`ppatrzyk/polska-geojson`** — granice województw/powiatów/gmin w GeoJSON (szybki start, potem PRG)
- **`GUGiK-ULDK`**, `wtyczki QGIS GUGiK` (`EnviroSolutions/gis-support-plugin`, „EnviroSolutions/uldk_gugik”) — gotowe listy usług WMS/WFS GUGiK i parametry zapytań — **kopalnia adresów usług**
- **`gis-support/…` — wtyczka „GIS Support” do QGIS** — zawiera katalog polskich WMS-ów (ponad 1000 usług z gmin/powiatów!) → można zaimportować do naszego katalogu warstw
- **`osm-polska`**, `openstreetmap-polska` — importy PRG/BDOT do OSM, narzędzia
- **`mapterhorn/mapterhorn`** (już używamy), `protomaps/basemaps` (MVT bazowa), `openfreemap` (używamy)
- **`imgw-api`** (różne klienty, np. `rafaltrojniak/imgw-api`) — opis formatów IMGW
- **`smogtok`/`luftdaten`** — sensor.community
- **`geo-polska`**, `teryt-scraper`, `PRG-processing` — przetwarzanie PRG/TERYT
- **`EIPA`** klienty — ładowarki
- Do zrobienia przez kolejną sesję: zweryfikować istnienie i licencje (`gh search repos "wms gugik"`, `"bdl gus api"`, `"imgw"`, `"gtfs polska"`).

---

## 4. Priorytety (co daje najwięcej warstw najmniejszym kosztem)

1. **Ogólny klient WMS/WMTS + katalog TOML** → jednym silnikiem ~80% warstw z sekcji 3 (GUGiK, GDOŚ, BDL, PIG, ISOK, NID, KIUT, MPZP, ARiMR, UKE). **Najważniejsze zadanie.**
2. **Kartogramy z GUS BDL API + PRG** → setki wskaźników statystycznych jednym mechanizmem.
3. **Punkty na żywo (GeoJSON feed)** → IMGW synop/hydro, GIOŚ, sensor.community, EIPA, GBFS, OpenSky, ostrzeżenia.
4. **Pola siatkowe z Open-Meteo** → pogoda/smog/pyłki z osią czasu.
5. **NMT GUGiK jako teren** + LOD2 budynki → jakość 3D nieosiągalna dla innych przeglądarek.
6. Warstwy czasowe (radar, archiwalne ortofoto) + porównywarka.
7. Importer dane.gov.pl / własnych plików (GeoJSON, KML, GPX, CSV, SHP) jako warstw użytkownika.

---

## 5. Etapy realizacji (każdy = PR do `main`, wydanie po zielonym CI)

### Etap 0 — Rebranding i porządki (0.1.0)
- Założyć repo `BlackSingularities/polskie-studio-map`, ustawić `origin`, sekrety podpisu w Actions.
- Nazwa w `Cargo.toml` (`polskie-studio-map`, bin `psm`), wersja `0.1.0`, ikony, `build/psm.nsi` (nowy GUID/AppId, `%APPDATA%\PSM`, folder danych `PSM-dane`, bez usuwania GRANI 1.x), `updater.rs` FEED, projekt w portfolio redzikowski.com.
- Zaktualizować `CLAUDE.md`, `README.md`.
- **Usunąć**: `graph.rs`, `ui/profile.rs`, eksport tras z `gpx.rs` (import GPX jako warstwa użytkownika zostaje na później), UI planowania trasy z `atlas.rs`/`panels.rs`.
- **Ograniczyć do PL**: `coverage.json` tylko PL, usunąć SK/CZ z `national.rs`, bbox kamery, maska.

### Etap 1 — Rdzeń warstw (0.2.0)
- `src/layers/{mod.rs, catalog.rs, wms.rs, wmts.rs, xyz.rs, legend.rs, query.rs}`, `assets/layers/*.toml`.
- Klucz cache `layer_id/z/x/y` w `store.rs`, limity per warstwa, stan włączenia/przezroczystości w `settings.rs`.
- Kompozycja rastrów w `render/tiles.rs` + `terrain.wgsl` (texture array, opacity, blend).
- Panel Warstwy + legenda + „i” + wyszukiwarka; presety.
- Pierwsze ~40 warstw WMS: ortofoto (bieżące + archiwalne), NMT hillshade, BDOT10k, PRG, działki, MPZP, KIUT, GDOŚ (8), BDL (5), ISOK (4), PIG (5), NID (3), UKE, ARiMR.

### Etap 2 — Kliknij i dowiedz się (0.3.0)
- GetFeatureInfo dla wszystkich aktywnych warstw → zbiorczy panel; ULDK (działka), wysokość z usługi GUGiK, adres z PRPOG; wyszukiwarka adresów/działek.

### Etap 3 — Dane na żywo (0.4.0)
- `src/feeds/` — IMGW synop/hydro/ostrzeżenia, GIOŚ, sensor.community, EIPA, GBFS, OpenSky, RCB; odświeżanie w tle, symbole skalowane wartością, popup z historią (mini-wykres egui).

### Etap 4 — Statystyka (0.5.0)
- PRG gmin/powiatów wbudowane (uproszczone), klient GUS BDL API (wybór tematu → zmienna → rok), klasyfikacja (kwantyle/naturalne przerwy), legenda, słupki 3D (wysokość = wartość) — efekt „wow” w 3D. Siatka 1 km NSP 2021. Wybory PKW.

### Etap 5 — Meteo i czas (0.6.0)
- Open-Meteo siatka nad PL (np. 0,1° → ~6 tys. punktów, pobierane paczkami), shader rampy, oś czasu ±48 h; radar IMGW animowany; satelita EUMETSAT; animacja wiatru (cząstki).

### Etap 6 — Lepsze 3D (0.7.0)
- NMT GUGiK 1 m jako teren (bake z `opendata.geoportal.gov.pl` do naszych paczek; Mapterhorn jako zapas), NMPT jako opcja „drzewa i budynki z lidaru”, budynki LOD2, potencjał PV dachów z `sun.rs`.

### Etap 7 — Warstwy użytkownika i importy (0.8.0)
- Pliki GeoJSON/KML/GPX/CSV/SHP, dowolny adres WMS/WMTS (z GetCapabilities → lista warstw), import katalogu gminnych WMS (lista z wtyczki GIS Support), wyszukiwarka zbiorów dane.gov.pl.

### Etap 8 — Narzędzia (0.9.0 → 1.0.0)
- Porównywarka (swipe), przekrój terenu, pomiary, eksport widoku PNG/wydruk z legendą i atrybucją, zakładki widoków, izochrony z GTFS/OSM (zamiennik tras).

---

## 6. Ryzyka i zasady
- **Licencje i atrybucja** — każda warstwa ma `attribution`, wyświetlane w rogu dla aktywnych. Blitzortung, Open-Meteo, OpenSky: tylko niekomercyjnie — oznaczyć; jeśli produkt kiedyś komercyjny → wyłączyć lub płatny plan.
- **Obciążenie serwisów państwowych** — limit równoległych żądań per host (np. 4), cache na dysku, poważny `User-Agent` z kontaktem (już jest w `download.rs`), brak prefetchu dużych obszarów WMS.
- **EPSG:2180** — wiele usług GUGiK/GDOŚ działa najlepiej w PUWG 1992; dodać `proj`-lite (transformacja 2180↔3857 jest prosta: Gauss–Krüger na GRS80) i reprojekcję kafli lub prosić WMS w 3857 tam, gdzie wspiera.
- **Niestabilne endpointy** — katalog TOML aktualizowany zdalnie, status warstwy „serwis niedostępny” zamiast błędu.
- **Wydajność** — przy wielu warstwach: limit aktywnych rastrów, kompozycja CPU dla nadmiaru, LOD dla punktów (klastrowanie).
- **Nic spoza Polski** — każdy nowy feed globalny musi mieć filtr bbox/maski PL przed pokazaniem.

## 7. Następne kroki dla kolejnej sesji
1. Etap 0 (rebranding + usunięcie tras + tylko PL) — po założeniu repo na GitHubie przez użytkownika.
2. Zweryfikować adresy z sekcji 3 (GetCapabilities) i zapisać pierwsze `assets/layers/*.toml`.
3. Etap 1.
