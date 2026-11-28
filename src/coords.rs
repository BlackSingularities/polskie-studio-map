//! Układy współrzędnych: UTM (WGS 84, szeregi Krügera – dokładność poniżej milimetra w pasie),
//! MGRS, polskie układy PL-1992 i PL-2000 (odwzorowanie Gaussa-Krügera), zapis w wybranym
//! formacie i rozpoznawanie współrzędnych wpisanych przez użytkownika.

use std::sync::atomic::{AtomicU8, Ordering};

use serde::{Deserialize, Serialize};

/// Format współrzędnych w interfejsie.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum CoordFormat {
    /// 49°13′55,2″N  19°58′54,1″E
    #[default]
    Dms,
    /// 49.23200, 19.98170
    Decimal,
    /// 34U 427123 5453210
    Utm,
    /// 34U DV 27123 53210
    Mgrs,
    /// X 167 123  Y 567 890 (PL-1992, EPSG:2180 – mapy urzędowe)
    Pl1992,
    /// X 5 453 210  Y 7 427 123 (PL-2000 – mapa zasadnicza i ewidencja gruntów)
    Pl2000,
}

impl CoordFormat {
    pub const ALL: [CoordFormat; 6] = [
        CoordFormat::Dms,
        CoordFormat::Decimal,
        CoordFormat::Pl1992,
        CoordFormat::Pl2000,
        CoordFormat::Utm,
        CoordFormat::Mgrs,
    ];

    pub fn name(self) -> &'static str {
        match self {
            CoordFormat::Dms => "Stopnie, minuty, sekundy",
            CoordFormat::Decimal => "Stopnie dziesiętne",
            CoordFormat::Utm => "UTM",
            CoordFormat::Mgrs => "MGRS",
            CoordFormat::Pl1992 => "PL-1992 (EPSG:2180)",
            CoordFormat::Pl2000 => "PL-2000 (strefy 5–8)",
        }
    }

    /// Przykład zapisu (Pałac Kultury i Nauki).
    pub fn example(self) -> String {
        format_as(self, 21.00672, 52.23175)
    }

    fn code(self) -> u8 {
        self as u8
    }

    fn from_code(c: u8) -> Self {
        match c {
            1 => CoordFormat::Decimal,
            2 => CoordFormat::Utm,
            3 => CoordFormat::Mgrs,
            4 => CoordFormat::Pl1992,
            5 => CoordFormat::Pl2000,
            _ => CoordFormat::Dms,
        }
    }
}

static FORMAT: AtomicU8 = AtomicU8::new(0);

/// Format współrzędnych używany w całym interfejsie (z `Prefs::coord_format`).
pub fn set_format(f: CoordFormat) {
    FORMAT.store(f.code(), Ordering::Relaxed);
}

pub fn format() -> CoordFormat {
    CoordFormat::from_code(FORMAT.load(Ordering::Relaxed))
}

// ---------------------------------------------------------------- odwzorowanie Gaussa-Krügera

const A: f64 = 6_378_137.0;
const F: f64 = 1.0 / 298.257_223_563;
const K0: f64 = 0.9996;
const FALSE_E: f64 = 500_000.0;
const FALSE_N_SOUTH: f64 = 10_000_000.0;
/// Litery pasów szerokości co 8° od 80° S (X obejmuje 72–84° N).
const BANDS: &[u8] = b"CDEFGHJKLMNPQRSTUVWXX";

/// Punkt w układzie UTM.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Utm {
    pub zone: u8,
    /// Litera pasa szerokości (C…X).
    pub band: char,
    pub easting: f64,
    pub northing: f64,
}

impl Utm {
    pub fn north(&self) -> bool {
        self.band >= 'N'
    }
}

struct Series {
    a_big: f64,
    e: f64,
    alpha: [f64; 6],
    beta: [f64; 6],
}

fn series() -> Series {
    let n = F / (2.0 - F);
    let (n2, n3, n4, n5, n6) = (n * n, n.powi(3), n.powi(4), n.powi(5), n.powi(6));
    Series {
        a_big: A / (1.0 + n) * (1.0 + n2 / 4.0 + n4 / 64.0 + n6 / 256.0),
        e: (F * (2.0 - F)).sqrt(),
        alpha: [
            n / 2.0 - 2.0 / 3.0 * n2 + 5.0 / 16.0 * n3 + 41.0 / 180.0 * n4 - 127.0 / 288.0 * n5 + 7891.0 / 37800.0 * n6,
            13.0 / 48.0 * n2 - 3.0 / 5.0 * n3 + 557.0 / 1440.0 * n4 + 281.0 / 630.0 * n5 - 1983433.0 / 1935360.0 * n6,
            61.0 / 240.0 * n3 - 103.0 / 140.0 * n4 + 15061.0 / 26880.0 * n5 + 167603.0 / 181440.0 * n6,
            49561.0 / 161280.0 * n4 - 179.0 / 168.0 * n5 + 6601661.0 / 7257600.0 * n6,
            34729.0 / 80640.0 * n5 - 3418889.0 / 1995840.0 * n6,
            212378941.0 / 319334400.0 * n6,
        ],
        beta: [
            n / 2.0 - 2.0 / 3.0 * n2 + 37.0 / 96.0 * n3 - 1.0 / 360.0 * n4 - 81.0 / 512.0 * n5 + 96199.0 / 604800.0 * n6,
            1.0 / 48.0 * n2 + 1.0 / 15.0 * n3 - 437.0 / 1440.0 * n4 + 46.0 / 105.0 * n5 - 1118711.0 / 3870720.0 * n6,
            17.0 / 480.0 * n3 - 37.0 / 840.0 * n4 - 209.0 / 4480.0 * n5 + 5569.0 / 90720.0 * n6,
            4397.0 / 161280.0 * n4 - 11.0 / 504.0 * n5 - 830251.0 / 7257600.0 * n6,
            4583.0 / 161280.0 * n5 - 108847.0 / 3991680.0 * n6,
            20648693.0 / 638668800.0 * n6,
        ],
    }
}

/// Odwzorowanie poprzeczne Merkatora względem południka `lon0` w skali 1:
/// (x – na wschód od południka, y – na północ od równika) w metrach.
fn tm_forward(lon: f64, lat: f64, lon0: f64) -> (f64, f64) {
    let s = series();
    let phi = lat.to_radians();
    let lam = ((lon - lon0 + 180.0).rem_euclid(360.0) - 180.0).to_radians();
    let tau = phi.tan();
    let sigma = (s.e * (s.e * tau / (1.0 + tau * tau).sqrt()).atanh()).sinh();
    let tau_p = tau * (1.0 + sigma * sigma).sqrt() - sigma * (1.0 + tau * tau).sqrt();
    let (cl, sl) = (lam.cos(), lam.sin());
    let xi_p = tau_p.atan2(cl);
    let eta_p = (sl / (tau_p * tau_p + cl * cl).sqrt()).asinh();
    let mut xi = xi_p;
    let mut eta = eta_p;
    for (j, a) in s.alpha.iter().enumerate() {
        let k = 2.0 * (j + 1) as f64;
        xi += a * (k * xi_p).sin() * (k * eta_p).cosh();
        eta += a * (k * xi_p).cos() * (k * eta_p).sinh();
    }
    (s.a_big * eta, s.a_big * xi)
}

/// Odwrotność [`tm_forward`]: metry w skali 1 → (długość, szerokość).
fn tm_inverse(x: f64, y: f64, lon0: f64) -> (f64, f64) {
    let s = series();
    let eta = x / s.a_big;
    let xi = y / s.a_big;
    let mut xi_p = xi;
    let mut eta_p = eta;
    for (j, b) in s.beta.iter().enumerate() {
        let k = 2.0 * (j + 1) as f64;
        xi_p -= b * (k * xi).sin() * (k * eta).cosh();
        eta_p -= b * (k * xi).cos() * (k * eta).sinh();
    }
    let sh = eta_p.sinh();
    let (sx, cx) = (xi_p.sin(), xi_p.cos());
    let tau_p = sx / (sh * sh + cx * cx).sqrt();
    let e2 = s.e * s.e;
    let mut tau = tau_p;
    for _ in 0..12 {
        let sigma = (s.e * (s.e * tau / (1.0 + tau * tau).sqrt()).atanh()).sinh();
        let tau_i = tau * (1.0 + sigma * sigma).sqrt() - sigma * (1.0 + tau * tau).sqrt();
        let d = (tau_p - tau_i) / (1.0 + tau_i * tau_i).sqrt() * (1.0 + (1.0 - e2) * tau * tau) / ((1.0 - e2) * (1.0 + tau * tau).sqrt());
        tau += d;
        if d.abs() < 1e-12 {
            break;
        }
    }
    let lat = tau.atan().to_degrees();
    let lon = sh.atan2(cx).to_degrees() + lon0;
    ((lon + 180.0).rem_euclid(360.0) - 180.0, lat)
}

/// Pas UTM punktu (z wyjątkami dla Norwegii i Svalbardu); `None` poza zakresem 80° S – 84° N.
pub fn zone_of(lon: f64, lat: f64) -> Option<u8> {
    if !(-80.0..=84.0).contains(&lat) || !lon.is_finite() {
        return None;
    }
    let lon = (lon + 180.0).rem_euclid(360.0) - 180.0;
    let mut z = ((lon + 180.0) / 6.0).floor() as i32 + 1;
    if z > 60 {
        z = 60;
    }
    if (56.0..64.0).contains(&lat) && (3.0..12.0).contains(&lon) {
        z = 32;
    }
    if lat >= 72.0 {
        if (0.0..9.0).contains(&lon) {
            z = 31;
        } else if (9.0..21.0).contains(&lon) {
            z = 33;
        } else if (21.0..33.0).contains(&lon) {
            z = 35;
        } else if (33.0..42.0).contains(&lon) {
            z = 37;
        }
    }
    Some(z as u8)
}

pub fn band_of(lat: f64) -> char {
    let i = ((lat + 80.0) / 8.0).floor().clamp(0.0, (BANDS.len() - 1) as f64) as usize;
    BANDS[i] as char
}

fn central_meridian(zone: u8) -> f64 {
    (zone as f64 - 1.0) * 6.0 - 180.0 + 3.0
}

/// Stopnie → UTM we właściwym pasie.
pub fn to_utm(lon: f64, lat: f64) -> Option<Utm> {
    let zone = zone_of(lon, lat)?;
    Some(to_utm_in(lon, lat, zone))
}

/// Stopnie → UTM w podanym pasie (także poza jego granicami).
pub fn to_utm_in(lon: f64, lat: f64, zone: u8) -> Utm {
    let (x, y) = tm_forward(lon, lat, central_meridian(zone));
    let (x, y) = (K0 * x, K0 * y);
    Utm {
        zone,
        band: band_of(lat),
        easting: x + FALSE_E,
        northing: if lat < 0.0 { y + FALSE_N_SOUTH } else { y },
    }
}

/// UTM → stopnie (długość, szerokość). Półkulę wyznacza litera pasa.
pub fn from_utm(u: &Utm) -> (f64, f64) {
    from_utm_raw(u.zone, u.north(), u.easting, u.northing)
}

fn from_utm_raw(zone: u8, north: bool, easting: f64, northing: f64) -> (f64, f64) {
    let x = easting - FALSE_E;
    let y = if north { northing } else { northing - FALSE_N_SOUTH };
    tm_inverse(x / K0, y / K0, central_meridian(zone))
}

// ---------------------------------------------------------------- MGRS

const COLS: [&[u8]; 3] = [b"ABCDEFGH", b"JKLMNPQR", b"STUVWXYZ"];
const ROWS: &[u8] = b"ABCDEFGHJKLMNPQRSTUV";

/// UTM → MGRS z `digits` cyframi na oś (1–5; 5 = 1 m). Cyfry są obcinane, jak wymaga MGRS.
pub fn mgrs(u: &Utm, digits: usize) -> String {
    let digits = digits.clamp(1, 5);
    let e100 = (u.easting / 100_000.0).floor() as i64;
    let n100 = (u.northing / 100_000.0).floor() as i64;
    let set = (u.zone as usize - 1) % 3;
    let col = COLS[set][((e100 - 1).clamp(0, 7)) as usize] as char;
    let off = if u.zone % 2 == 0 { 5 } else { 0 };
    let row = ROWS[((n100 + off).rem_euclid(20)) as usize] as char;
    let div = 10f64.powi(5 - digits as i32);
    let e = ((u.easting.rem_euclid(100_000.0)) / div).floor() as i64;
    let n = ((u.northing.rem_euclid(100_000.0)) / div).floor() as i64;
    format!("{}{} {}{} {:0w$} {:0w$}", u.zone, u.band, col, row, e, n, w = digits)
}

/// MGRS → stopnie (długość, szerokość) – środek wskazanego kwadratu.
pub fn from_mgrs(s: &str) -> Option<(f64, f64)> {
    let t: String = s.chars().filter(|c| !c.is_whitespace()).collect::<String>().to_uppercase();
    if !t.is_ascii() {
        return None;
    }
    let b = t.as_bytes();
    let nz = b.iter().take_while(|c| c.is_ascii_digit()).count();
    if !(1..=2).contains(&nz) || b.len() < nz + 3 {
        return None;
    }
    let zone: u8 = t[..nz].parse().ok()?;
    if !(1..=60).contains(&zone) {
        return None;
    }
    let band = b[nz] as char;
    let bi = BANDS.iter().position(|&c| c as char == band)?;
    let (cl, rl) = (b[nz + 1], b[nz + 2]);
    let digits = &t[nz + 3..];
    if digits.len() % 2 != 0 || digits.len() > 10 || !digits.bytes().all(|c| c.is_ascii_digit()) {
        return None;
    }
    let set = (zone as usize - 1) % 3;
    let col = COLS[set].iter().position(|&c| c == cl)? as f64 + 1.0;
    let off = if zone % 2 == 0 { 5 } else { 0 };
    let row = (ROWS.iter().position(|&c| c == rl)? as i64 - off).rem_euclid(20) as f64;
    let k = digits.len() / 2;
    let (de, dn) = if k == 0 {
        (50_000.0, 50_000.0)
    } else {
        let unit = 10f64.powi(5 - k as i32);
        (
            digits[..k].parse::<f64>().ok()? * unit + unit / 2.0,
            digits[k..].parse::<f64>().ok()? * unit + unit / 2.0,
        )
    };
    let easting = col * 100_000.0 + de;
    // cykl liter wierszy powtarza się co 2000 km: dobieramy go do pasa szerokości
    let band_lat = -80.0 + bi as f64 * 8.0;
    let band_n = to_utm_in(central_meridian(zone), band_lat, zone).northing;
    let base = (band_n / 100_000.0).floor() * 100_000.0;
    let mut northing = row * 100_000.0 + dn;
    while northing < base {
        northing += 2_000_000.0;
    }
    let north = band >= 'N';
    let (lon, lat) = from_utm_raw(zone, north, easting, northing);
    (lat.is_finite() && lon.is_finite()).then_some((lon, lat))
}

// ---------------------------------------------------------------- zapis

fn dec(v: f64, d: usize) -> String {
    format!("{v:.d$}").replace('.', ",")
}

fn dms(v: f64, pos: char, neg: char) -> String {
    let a = v.abs();
    // zaokrąglenie do 0,1″ przed rozbiciem, żeby nie wyszło 60″
    let tenths = (a * 36_000.0).round() as i64;
    let d = tenths / 36_000;
    let m = (tenths % 36_000) / 600;
    let s = (tenths % 600) as f64 / 10.0;
    format!("{}°{:02}′{:0>4}″{}", d, m, dec(s, 1), if v >= 0.0 { pos } else { neg })
}

/// Zapis w podanym formacie (UTM/MGRS poza zakresem – stopnie dziesiętne).
pub fn format_as(f: CoordFormat, lon: f64, lat: f64) -> String {
    match f {
        CoordFormat::Dms => format!("{}  {}", dms(lat, 'N', 'S'), dms(lon, 'E', 'W')),
        CoordFormat::Decimal => format!("{lat:.5}, {lon:.5}"),
        CoordFormat::Utm => match to_utm(lon, lat) {
            Some(u) => format!("{}{} {:.0} {:.0}", u.zone, u.band, u.easting.floor(), u.northing.floor()),
            None => format!("{lat:.5}, {lon:.5}"),
        },
        CoordFormat::Mgrs => match to_utm(lon, lat) {
            Some(u) => mgrs(&u, 5),
            None => format!("{lat:.5}, {lon:.5}"),
        },
        CoordFormat::Pl1992 => {
            let (x, y) = to_pl1992(lon, lat);
            format!("X {}  Y {}", group(x), group(y))
        }
        CoordFormat::Pl2000 => {
            let (x, y, _) = to_pl2000(lon, lat);
            format!("X {}  Y {}", group(x), group(y))
        }
    }
}

/// Zapis w formacie wybranym w ustawieniach.
pub fn format_pos(lon: f64, lat: f64) -> String {
    format_as(format(), lon, lat)
}

// ---------------------------------------------------------------- odczyt

/// Rozpoznanie współrzędnych w tekście: stopnie dziesiętne („49.232, 19.98”, „49,232 19,98”),
/// stopnie-minuty-sekundy („49°13′55″N 19°58′54″E”), UTM („34U 426815 5453756”) i MGRS
/// („34U DV 26815 53756”). Wynik: (długość, szerokość).
pub fn parse(text: &str) -> Option<(f64, f64)> {
    let t = text.trim();
    if t.is_empty() {
        return None;
    }
    parse_utm(t)
        .or_else(|| from_mgrs(t))
        .or_else(|| parse_pl(t))
        .or_else(|| parse_degrees(t))
        .filter(|(lon, lat)| lat.abs() <= 90.0 && lon.abs() <= 180.0)
}

fn parse_utm(t: &str) -> Option<(f64, f64)> {
    let up = t.to_uppercase();
    let parts: Vec<&str> = up.split(|c: char| c.is_whitespace() || c == ',' || c == ';').filter(|s| !s.is_empty()).collect();
    // „34U 426815 5453756”, „34 U 426815E 5453756N”
    let (head, rest): (String, &[&str]) = match parts.as_slice() {
        [z, b, rest @ ..] if z.chars().all(|c| c.is_ascii_digit()) && b.len() == 1 && rest.len() == 2 => (format!("{z}{b}"), rest),
        [zb, rest @ ..] if rest.len() == 2 => (zb.to_string(), rest),
        _ => return None,
    };
    let nz = head.bytes().take_while(|c| c.is_ascii_digit()).count();
    if !(1..=2).contains(&nz) || head.len() != nz + 1 {
        return None;
    }
    let zone: u8 = head[..nz].parse().ok()?;
    let band = head[nz..].chars().next()?;
    if !(1..=60).contains(&zone) || !BANDS.contains(&(band as u8)) {
        return None;
    }
    let num = |s: &str| -> Option<f64> {
        let s = s.trim_end_matches(['E', 'N', 'M']);
        if s.is_empty() || !s.bytes().all(|c| c.is_ascii_digit() || c == b'.') {
            return None;
        }
        s.parse().ok()
    };
    let easting = num(rest[0])?;
    let northing = num(rest[1])?;
    // MGRS bez spacji w liczbach nie przejdzie tędy: wschodnia ma 6 cyfr, północna do 8
    if !(100_000.0..1_000_000.0).contains(&easting) || !(0.0..10_000_000.0).contains(&northing) {
        return None;
    }
    Some(from_utm(&Utm { zone, band, easting, northing }))
}

/// Stopnie: liczby z opcjonalnymi literami półkul.
fn parse_degrees(t: &str) -> Option<(f64, f64)> {
    // przecinek z odstępem rozdziela współrzędne, sam przecinek to przecinek dziesiętny
    let s = t.replace(", ", " ").replace(';', " ");
    let commas = s.matches(',').count();
    let s = if commas == 1 && !s.contains(' ') {
        // „49.2,19.9” – przecinek rozdziela
        s.replace(',', " ")
    } else {
        s.replace(',', ".")
    };
    // tokeny: liczby i litery półkul
    #[derive(Debug)]
    enum Tok {
        Num(f64),
        Hemi(char),
    }
    let mut toks = Vec::new();
    let mut cur = String::new();
    let flush = |cur: &mut String, toks: &mut Vec<Tok>| -> Option<()> {
        if !cur.is_empty() {
            toks.push(Tok::Num(cur.parse().ok()?));
            cur.clear();
        }
        Some(())
    };
    for c in s.chars() {
        match c {
            '0'..='9' | '.' => cur.push(c),
            '-' | '−' if cur.is_empty() => cur.push('-'),
            'N' | 'S' | 'E' | 'W' | 'n' | 's' | 'e' | 'w' => {
                flush(&mut cur, &mut toks)?;
                toks.push(Tok::Hemi(c.to_ascii_uppercase()));
            }
            '°' | '′' | '″' | '\'' | '"' | ' ' | '\t' | 'º' => flush(&mut cur, &mut toks)?,
            _ => return None,
        }
    }
    flush(&mut cur, &mut toks)?;
    // grupy liczb zamykane literą półkuli (albo litera przed liczbami)
    let mut groups: Vec<(Vec<f64>, Option<char>)> = vec![(Vec::new(), None)];
    let mut leading = false;
    for (i, tk) in toks.iter().enumerate() {
        match tk {
            Tok::Num(v) => groups.last_mut()?.0.push(*v),
            Tok::Hemi(h) => {
                let last = groups.last_mut()?;
                if last.0.is_empty() && last.1.is_none() {
                    last.1 = Some(*h);
                    leading = true;
                } else if leading {
                    groups.push((Vec::new(), Some(*h)));
                } else {
                    last.1 = Some(*h);
                    if i + 1 < toks.len() {
                        groups.push((Vec::new(), None));
                    }
                }
            }
        }
    }
    groups.retain(|g| !g.0.is_empty());
    let has_letters = groups.iter().any(|g| g.1.is_some());
    let groups: Vec<(Vec<f64>, Option<char>)> = if has_letters {
        groups
    } else {
        // bez liter: liczby dzielone po równo (szerokość pierwsza)
        let nums: Vec<f64> = groups.into_iter().flat_map(|g| g.0).collect();
        if nums.len() % 2 != 0 || nums.is_empty() || nums.len() > 6 {
            return None;
        }
        let h = nums.len() / 2;
        vec![(nums[..h].to_vec(), None), (nums[h..].to_vec(), None)]
    };
    if groups.len() != 2 {
        return None;
    }
    let val = |g: &(Vec<f64>, Option<char>)| -> Option<f64> {
        let n = &g.0;
        if n.is_empty() || n.len() > 3 {
            return None;
        }
        let neg = n[0] < 0.0 || n[0] == 0.0 && n[0].is_sign_negative();
        let mut v = n[0].abs();
        if let Some(m) = n.get(1) {
            if *m >= 60.0 || n[0].fract() != 0.0 {
                return None;
            }
            v += m / 60.0;
        }
        if let Some(s) = n.get(2) {
            if *s >= 60.0 || n[1].fract() != 0.0 {
                return None;
            }
            v += s / 3600.0;
        }
        let v = if neg { -v } else { v };
        Some(match g.1 {
            Some('S') | Some('W') => -v.abs(),
            _ => v,
        })
    };
    let (a, b) = (val(&groups[0])?, val(&groups[1])?);
    let a_lon = matches!(groups[0].1, Some('E') | Some('W'));
    let b_lat = matches!(groups[1].1, Some('N') | Some('S'));
    if a_lon || b_lat {
        Some((a, b))
    } else {
        Some((b, a))
    }
}

// ---------------------------------------------------------------- układy polskie

/// PL-1992 (EPSG:2180): południk 19°, skala 0,9993, przesunięcia 500 km i −5300 km.
/// Elipsoida GRS 80 różni się od WGS 84 o ułamki milimetra – liczymy szeregami WGS 84.
const PL92_K: f64 = 0.9993;
const PL92_E: f64 = 500_000.0;
const PL92_N: f64 = -5_300_000.0;
/// PL-2000 (EPSG:2176–2179): strefy 5–8 co 3° (południki 15°, 18°, 21°, 24°), skala 0,999923.
const PL20_K: f64 = 0.999_923;

/// Stopnie → PL-1992: (X – na północ, Y – na wschód), jak w polskiej geodezji.
pub fn to_pl1992(lon: f64, lat: f64) -> (f64, f64) {
    let (x, y) = tm_forward(lon, lat, 19.0);
    (PL92_K * y + PL92_N, PL92_K * x + PL92_E)
}

/// PL-1992 (X na północ, Y na wschód) → (długość, szerokość).
pub fn from_pl1992(x: f64, y: f64) -> (f64, f64) {
    tm_inverse((y - PL92_E) / PL92_K, (x - PL92_N) / PL92_K, 19.0)
}

/// Strefa PL-2000 dla długości geograficznej (5–8).
pub fn pl2000_zone(lon: f64) -> u8 {
    if lon < 16.5 {
        5
    } else if lon < 19.5 {
        6
    } else if lon < 22.5 {
        7
    } else {
        8
    }
}

/// Stopnie → PL-2000: (X, Y z numerem strefy na początku, strefa).
pub fn to_pl2000(lon: f64, lat: f64) -> (f64, f64, u8) {
    let zone = pl2000_zone(lon);
    let (x, y) = tm_forward(lon, lat, zone as f64 * 3.0);
    (PL20_K * y, PL20_K * x + zone as f64 * 1_000_000.0 + 500_000.0, zone)
}

/// PL-2000 (X, Y ze strefą) → (długość, szerokość); `None`, gdy Y nie zaczyna się od strefy 5–8.
pub fn from_pl2000(x: f64, y: f64) -> Option<(f64, f64)> {
    let zone = (y / 1_000_000.0).floor();
    if !(5.0..=8.0).contains(&zone) {
        return None;
    }
    let e = y - zone * 1_000_000.0 - 500_000.0;
    Some(tm_inverse(e / PL20_K, x / PL20_K, zone * 3.0))
}

/// Liczba metrów z wąskim odstępem co trzy cyfry: „5 453 210”.
fn group(v: f64) -> String {
    let n = v.round() as i64;
    let digits = n.unsigned_abs().to_string();
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i) % 3 == 0 {
            out.push('\u{2009}');
        }
        out.push(c);
    }
    if n < 0 {
        format!("−{out}")
    } else {
        out
    }
}

/// Czy punkt leży w Polsce z zapasem (sprawdzenie odczytanych współrzędnych płaskich).
fn near_poland(lon: f64, lat: f64) -> bool {
    (13.5..=24.8).contains(&lon) && (48.7..=55.3).contains(&lat)
}

/// Współrzędne płaskie PL-2000 albo PL-1992: dwie liczby metrów, najpierw X (północ), potem Y
/// (wschód), jak w polskiej geodezji; dopuszczalne litery X i Y, dwukropki i znaki równości.
/// Gdy kolejność jest odwrotna, a tylko ona daje punkt w Polsce – przyjmujemy odwrotną.
fn parse_pl(t: &str) -> Option<(f64, f64)> {
    let s = t.replace(", ", " ").replace(';', " ");
    let mut nums: Vec<f64> = Vec::new();
    let mut cur = String::new();
    for c in s.chars().chain(std::iter::once(' ')) {
        match c {
            '0'..='9' => cur.push(c),
            '.' | ',' if !cur.is_empty() => cur.push('.'),
            // wąskie i twarde odstępy w zapisie programu nie dzielą liczby
            '\u{2009}' | '\u{a0}' | '\u{202f}' => {}
            ' ' | '\t' | ':' | '=' | 'x' | 'X' | 'y' | 'Y' => {
                if !cur.is_empty() {
                    nums.push(cur.trim_end_matches('.').parse().ok()?);
                    cur.clear();
                }
            }
            _ => return None,
        }
    }
    let big: Vec<f64> = nums.into_iter().filter(|v| *v >= 100_000.0).collect();
    let &[a, b] = big.as_slice() else { return None };
    let candidates = [(a, b), (b, a)];
    // PL-2000: Y ma 7 cyfr i zaczyna się od numeru strefy
    for (x, y) in candidates {
        if (4_000_000.0..7_000_000.0).contains(&x) && (5_000_000.0..9_000_000.0).contains(&y) {
            if let Some(p) = from_pl2000(x, y).filter(|p| near_poland(p.0, p.1)) {
                return Some(p);
            }
        }
    }
    for (x, y) in candidates {
        if (100_000.0..1_000_000.0).contains(&x) && (100_000.0..1_000_000.0).contains(&y) {
            let (lon, lat) = from_pl1992(x, y);
            if near_poland(lon, lat) {
                return Some((lon, lat));
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn utm_reference_points() {
        // równik na południku środkowym pasa 31
        let u = to_utm(3.0, 0.0).unwrap();
        assert_eq!((u.zone, u.band), (31, 'N'));
        assert!((u.easting - 500_000.0).abs() < 1e-6 && u.northing.abs() < 1e-6, "{u:?}");
        // wieża Eiffla: ok. 31U 448252 5411933 (DQ)
        let u = to_utm(2.2945, 48.8582).unwrap();
        assert_eq!((u.zone, u.band), (31, 'U'));
        assert!((u.easting - 448_252.0).abs() < 20.0 && (u.northing - 5_411_933.0).abs() < 20.0, "{u:?}");
        assert!(mgrs(&u, 5).starts_with("31U DQ "), "{}", mgrs(&u, 5));
    }

    #[test]
    fn utm_roundtrip() {
        for (lon, lat) in [(19.9817, 49.232), (14.1, 54.8), (24.0, 49.0), (-70.5, -33.4), (151.2, -33.9), (5.0, 60.0), (15.0, 78.0)] {
            let u = to_utm(lon, lat).unwrap();
            let (lo, la) = from_utm(&u);
            assert!((lo - lon).abs() < 1e-8 && (la - lat).abs() < 1e-8, "{lon} {lat} → {u:?} → {lo} {la}");
            let (lo, la) = from_mgrs(&mgrs(&u, 5)).unwrap();
            assert!((lo - lon).abs() < 3e-5 && (la - lat).abs() < 3e-5, "{lon} {lat} → {} → {lo} {la}", mgrs(&u, 5));
        }
        assert_eq!(zone_of(7.0, 60.0), Some(32));
        assert_eq!(zone_of(20.0, 78.0), Some(33));
        assert_eq!(zone_of(0.0, 85.0), None);
    }

    #[test]
    fn pl1992_reference() {
        // na południku 19° Y = 500 km; X = 0,9993 · łuk południka − 5300 km (52° N ≈ 459,3 km)
        let (x, y) = to_pl1992(19.0, 52.0);
        assert!((y - 500_000.0).abs() < 1e-6, "{y}");
        assert!((x - 459_300.0).abs() < 500.0, "{x}");
        // Pałac Kultury i Nauki: ok. X 487 000, Y 637 100
        let (x, y) = to_pl1992(21.00672, 52.23175);
        assert!((x - 487_000.0).abs() < 600.0 && (y - 637_100.0).abs() < 600.0, "{x} {y}");
        for (lon, lat) in [(14.2, 53.9), (19.98, 49.23), (24.1, 50.8), (18.6, 54.8)] {
            let (x, y) = to_pl1992(lon, lat);
            let (lo, la) = from_pl1992(x, y);
            assert!((lo - lon).abs() < 1e-8 && (la - lat).abs() < 1e-8, "{lon} {lat}");
        }
    }

    #[test]
    fn pl2000_reference() {
        let (x, y, z) = to_pl2000(21.0, 52.0);
        assert_eq!(z, 7);
        assert!((y - 7_500_000.0).abs() < 1e-6, "{y}");
        assert!((x - 5_762_900.0).abs() < 1_000.0, "{x}");
        for (lon, lat) in [(14.2, 53.9), (17.0, 51.1), (19.98, 49.23), (23.9, 50.8)] {
            let (x, y, _) = to_pl2000(lon, lat);
            let (lo, la) = from_pl2000(x, y).unwrap();
            assert!((lo - lon).abs() < 1e-8 && (la - lat).abs() < 1e-8, "{lon} {lat}");
        }
    }

    #[test]
    fn parse_inputs() {
        let near = |r: Option<(f64, f64)>, lon: f64, lat: f64| {
            let (a, b) = r.expect("brak wyniku");
            assert!((a - lon).abs() < 1e-3 && (b - lat).abs() < 1e-3, "{a} {b}");
        };
        near(parse("49.232, 19.9817"), 19.9817, 49.232);
        near(parse("49,232, 19,9817"), 19.9817, 49.232);
        near(parse("49,232 19,9817"), 19.9817, 49.232);
        near(parse("49.232,19.9817"), 19.9817, 49.232);
        near(parse("49°13′55,2″N 19°58′54,1″E"), 19.98169, 49.232);
        near(parse("N 49 13.92 E 19 58.90"), 19.98167, 49.232);
        near(parse("19.9817E 49.232N"), 19.9817, 49.232);
        let u = to_utm(19.9817, 49.232).unwrap();
        let utm = format_as(CoordFormat::Utm, 19.9817, 49.232);
        near(parse(&utm), 19.9817, 49.232);
        near(parse(&mgrs(&u, 5)), 19.9817, 49.232);
        near(parse(&mgrs(&u, 5).replace(' ', "")), 19.9817, 49.232);
        near(parse(&mgrs(&u, 4).to_lowercase()), 19.9817, 49.232);
        // układy polskie: zapis programu, same liczby, z literami i z przecinkiem dziesiętnym
        near(parse(&format_as(CoordFormat::Pl1992, 19.9817, 49.232)), 19.9817, 49.232);
        near(parse(&format_as(CoordFormat::Pl2000, 19.9817, 49.232)), 19.9817, 49.232);
        let (x, y) = to_pl1992(21.0, 52.2);
        near(parse(&format!("{x:.0} {y:.0}")), 21.0, 52.2);
        near(parse(&format!("X: {x:.2}  Y: {y:.2}").replace('.', ",")), 21.0, 52.2);
        let (x, y, _) = to_pl2000(17.03, 51.11);
        near(parse(&format!("x={x:.1} y={y:.1}")), 17.03, 51.11);
        near(parse(&format!("{y:.0} {x:.0}")), 17.03, 51.11);
        assert!(parse("Kasprowy Wierch").is_none());
        assert!(parse("Rysy").is_none());
        assert!(parse("2499").is_none());
        assert!(parse("ul. Marszałkowska 1").is_none());
    }

    #[test]
    fn dms_no_sixty() {
        assert_eq!(format_as(CoordFormat::Dms, 20.0, 49.999_999_9), "50°00′00,0″N  20°00′00,0″E");
    }
}
