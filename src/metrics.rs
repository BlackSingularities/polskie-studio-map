//! Czasy przejścia szlaków (PTTK, DIN 33466), suma podejść z histerezą, kolory szlaków.

use std::sync::atomic::{AtomicU32, AtomicU8, Ordering};

/// Model czasu przejścia: 0 – reguła PTTK, 1 – DIN 33466.
static MODEL: AtomicU8 = AtomicU8::new(0);
/// Tempo (mnożnik czasu × 1000): 800 szybkie … 1250 spokojne.
static PACE: AtomicU32 = AtomicU32::new(1000);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TimeModel {
    Pttk,
    Din,
}

impl TimeModel {
    pub fn label(self) -> &'static str {
        match self {
            TimeModel::Pttk => "reguła PTTK",
            TimeModel::Din => "DIN 33466",
        }
    }
}

pub fn set_model(m: TimeModel, pace: f64) {
    MODEL.store((m == TimeModel::Din) as u8, Ordering::Relaxed);
    PACE.store((pace * 1000.0).round() as u32, Ordering::Relaxed);
}

pub fn model() -> (TimeModel, f64) {
    (
        if MODEL.load(Ordering::Relaxed) == 1 {
            TimeModel::Din
        } else {
            TimeModel::Pttk
        },
        PACE.load(Ordering::Relaxed) as f64 / 1000.0,
    )
}

pub fn din_time(horiz: f64, up: f64, down: f64) -> f64 {
    let th = horiz / 4000.0;
    let tv = up / 300.0 + down / 500.0;
    th.max(tv) + th.min(tv) / 2.0
}

/// Czas przejścia (h). PTTK: 15 min/km + 1 min/10 m podejścia + 1 min/17 m zejścia.
pub fn walk_time(horiz: f64, up: f64, down: f64) -> f64 {
    let (m, pace) = model();
    let base = match m {
        TimeModel::Din => din_time(horiz, up, down),
        TimeModel::Pttk => horiz / 4000.0 + up / 600.0 + down / 1000.0,
    };
    base * pace
}

/// Mnożnik czasu dla skali trudności SAC (T1–T6).
pub fn sac_factor(sac: u8) -> f64 {
    [1.0, 1.0, 1.03, 1.12, 1.28, 1.5, 1.8]
        .get(sac as usize)
        .copied()
        .unwrap_or(1.0)
}

pub const SAC_LABEL: [&str; 7] = [
    "—",
    "T1 turystyczna",
    "T2 górska",
    "T3 wymagająca",
    "T4 wysokogórska",
    "T5 wymagająca wysokogórska",
    "T6 trudna wysokogórska",
];

/// Suma podejść i zejść z histerezą (eliminuje szum modelu terenu).
pub fn climb(e: &[f32]) -> (f64, f64) {
    if e.is_empty() {
        return (0.0, 0.0);
    }
    let (mut up, mut down) = (0.0f64, 0.0f64);
    let mut r = e[0] as f64;
    for &v in &e[1..] {
        let d = v as f64 - r;
        if d >= 2.5 {
            up += d;
            r = v as f64;
        } else if d <= -2.5 {
            down -= d;
            r = v as f64;
        }
    }
    let d = *e.last().unwrap() as f64 - r;
    if d > 0.0 {
        up += d
    } else {
        down -= d
    }
    (up, down)
}

/// Kolory szlaków (PTTK i inne).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[repr(u8)]
pub enum TrailColor {
    Red,
    Blue,
    Green,
    Yellow,
    Black,
    Other,
    Offtrail,
    Orange,
    Purple,
    Brown,
}

impl TrailColor {
    pub fn parse(s: &str) -> TrailColor {
        match s {
            "red" => TrailColor::Red,
            "blue" => TrailColor::Blue,
            "green" => TrailColor::Green,
            "yellow" => TrailColor::Yellow,
            "black" => TrailColor::Black,
            "orange" => TrailColor::Orange,
            "purple" => TrailColor::Purple,
            "brown" => TrailColor::Brown,
            _ => TrailColor::Other,
        }
    }

    pub fn rgb(self) -> [u8; 3] {
        match self {
            TrailColor::Red => [232, 58, 48],
            TrailColor::Blue => [44, 116, 232],
            TrailColor::Green => [38, 170, 76],
            TrailColor::Yellow => [246, 205, 40],
            TrailColor::Black => [24, 24, 28],
            TrailColor::Other => [214, 196, 160],
            TrailColor::Offtrail => [255, 140, 60],
            TrailColor::Orange => [245, 130, 32],
            TrailColor::Purple => [176, 96, 214],
            TrailColor::Brown => [140, 90, 50],
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            TrailColor::Red => "czerwony",
            TrailColor::Blue => "niebieski",
            TrailColor::Green => "zielony",
            TrailColor::Yellow => "żółty",
            TrailColor::Black => "czarny",
            TrailColor::Other => "inny",
            TrailColor::Offtrail => "poza szlakiem",
            TrailColor::Orange => "pomarańczowy",
            TrailColor::Purple => "fioletowy",
            TrailColor::Brown => "brązowy",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pttk_time_matches_rule() {
        set_model(TimeModel::Pttk, 1.0);
        // 4 km i 600 m podejścia: 1 h + 1 h
        assert!((walk_time(4000.0, 600.0, 0.0) - 2.0).abs() < 1e-9);
    }

    #[test]
    fn climb_hysteresis() {
        let e: Vec<f32> = (0..100).map(|i| 1000.0 + (i % 2) as f32).collect();
        let (up, down) = climb(&e);
        assert!(up < 2.0 && down < 2.0);
    }
}
