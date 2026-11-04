//! Analizy na lokalnej siatce wysokości: pole widoczności, linia wzroku,
//! cienie rzucane przez rzeźbę i statystyki wysokości względnej.

use rayon::prelude::*;

use crate::geo::EARTH_R;
use crate::heights::Grid;

/// Maska widoczności: 0 – niewidoczne, 1..255 – widoczne (zapas kąta, do cieniowania krawędzi).
pub struct Viewshed {
    pub mask: Vec<u8>,
    pub count: usize,
    pub area_km2: f64,
    pub observer: f32,
}

/// Widoczność promieniami do każdej komórki obwodu (odmiana XDraw) z krzywizną Ziemi i refrakcją.
pub fn viewshed(g: &Grid, ox: f64, oy: f64, eye: f32, target: f32) -> Viewshed {
    let (w, h) = (g.w, g.h);
    let oz = g.sample(ox, oy) + eye;
    let k = (1.0 - 0.13) / (2.0 * EARTH_R as f32);
    let mpp = g.mpp;
    let ray = |tx: f64, ty: f64, out: &mut Vec<(u32, u8)>| {
        let (dx, dy) = (tx - ox, ty - oy);
        let steps = dx.abs().max(dy.abs()).ceil();
        if steps < 1.0 {
            return;
        }
        let (sx, sy) = (dx / steps, dy / steps);
        let step_m = (sx.hypot(sy) as f32) * mpp;
        let mut max_tan = f32::NEG_INFINITY;
        for s in 1..=steps as usize {
            let (x, y) = (ox + sx * s as f64, oy + sy * s as f64);
            let (ix, iy) = (x.round() as isize, y.round() as isize);
            if ix < 0 || iy < 0 || ix >= w as isize || iy >= h as isize {
                break;
            }
            let d = s as f32 * step_m;
            let idx = iy as usize * w + ix as usize;
            let hh = g.data[idx] - d * d * k;
            let tan_t = (hh + target - oz) / d;
            if tan_t >= max_tan {
                let margin = (1.0 + (tan_t - max_tan).min(1e6) * d * 2.0).min(254.0);
                out.push((idx as u32, margin.max(1.0) as u8));
            }
            let tan_g = (hh - oz) / d;
            if tan_g > max_tan {
                max_tan = tan_g;
            }
        }
    };
    let mut border: Vec<(f64, f64)> = Vec::with_capacity(2 * (w + h));
    for x in 0..w {
        border.push((x as f64, 0.0));
        border.push((x as f64, (h - 1) as f64));
    }
    for y in 0..h {
        border.push((0.0, y as f64));
        border.push(((w - 1) as f64, y as f64));
    }
    let hits: Vec<Vec<(u32, u8)>> = border
        .par_chunks(64)
        .map(|c| {
            let mut out = Vec::new();
            for &(x, y) in c {
                ray(x, y, &mut out);
            }
            out
        })
        .collect();
    let mut mask = vec![0u8; w * h];
    for list in hits {
        for (i, m) in list {
            let v = &mut mask[i as usize];
            if *v < m {
                *v = m;
            }
        }
    }
    let (cx, cy) = (ox.round() as isize, oy.round() as isize);
    if cx >= 0 && cy >= 0 && (cx as usize) < w && (cy as usize) < h {
        mask[cy as usize * w + cx as usize] = 255;
    }
    let count = mask.iter().filter(|&&v| v > 0).count();
    Viewshed {
        area_km2: count as f64 * (mpp as f64).powi(2) / 1e6,
        mask,
        count,
        observer: oz,
    }
}

/// Linia wzroku między punktami siatki (oczy `eye` m nad terenem po obu stronach).
/// Zwraca `None` gdy widać, albo ułamek odległości do pierwszej przeszkody.
pub fn line_of_sight(g: &Grid, a: (f64, f64), b: (f64, f64), eye: f32) -> Option<f64> {
    let ha = g.sample(a.0, a.1) + eye;
    let hb = g.sample(b.0, b.1) + eye;
    let len_px = (b.0 - a.0).hypot(b.1 - a.1);
    let n = (len_px * 2.0).ceil().max(2.0) as usize;
    let total_m = len_px * g.mpp as f64;
    for i in 1..n {
        let f = i as f64 / n as f64;
        let ground = g.sample(a.0 + (b.0 - a.0) * f, a.1 + (b.1 - a.1) * f) as f64;
        let d = f * total_m;
        // krzywizna: spadek względem cięciwy
        let drop = d * (total_m - d) / (2.0 * EARTH_R) * (1.0 - 0.13);
        let line = ha as f64 + (hb - ha) as f64 * f - drop;
        if ground > line {
            return Some(f);
        }
    }
    None
}

/// Cienie rzucane przez rzeźbę (Słońce w kierunku `sun` = [wschód, góra, południe]).
/// Maska 0 – cień, 255 – słońce (miękka krawędź z zapasu kąta).
pub fn shadows(g: &Grid, sun: [f32; 3]) -> Vec<u8> {
    let (w, h) = (g.w, g.h);
    if sun[1] <= 0.0 {
        return vec![0; w * h];
    }
    let horiz = (sun[0] * sun[0] + sun[2] * sun[2]).sqrt().max(1e-6);
    let (dx, dy) = (sun[0] / horiz, sun[2] / horiz);
    let tan_sun = sun[1] / horiz;
    let mpp = g.mpp;
    let maxh = g.data.iter().cloned().fold(f32::MIN, f32::max);
    let mut out = vec![255u8; w * h];
    out.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
        for (x, o) in row.iter_mut().enumerate() {
            let h0 = g.data[y * w + x] + 0.5;
            let mut step = 1.0f32;
            let mut s = 1.0f32;
            let mut min_gap = f32::INFINITY;
            loop {
                let (px, py) = (x as f32 + dx * s, y as f32 + dy * s);
                if px < 0.0 || py < 0.0 || px > (w - 1) as f32 || py > (h - 1) as f32 {
                    break;
                }
                let ray = h0 + s * mpp * tan_sun;
                if ray > maxh + 5.0 {
                    break;
                }
                let gh = g.sample(px as f64, py as f64);
                let gap = (ray - gh) / (s * mpp);
                min_gap = min_gap.min(gap);
                if gap < -0.02 {
                    break;
                }
                s += step;
                step = (step * 1.04).min(8.0);
            }
            *o = if min_gap.is_infinite() {
                255
            } else {
                (((min_gap + 0.02) / 0.06).clamp(0.0, 1.0) * 255.0) as u8
            };
        }
    });
    out
}

/// Odsetek terenu wyżej i niżej od poziomu `ref_e` w promieniu `r_px` pikseli od punktu.
pub fn relative_stats(g: &Grid, cx: f64, cy: f64, ref_e: f32, r_px: f64) -> (f64, f64) {
    let (mut above, mut below, mut total) = (0usize, 0usize, 0usize);
    let r2 = r_px * r_px;
    let (x0, x1) = ((cx - r_px).max(0.0) as usize, (cx + r_px).min(g.w as f64 - 1.0) as usize);
    let (y0, y1) = ((cy - r_px).max(0.0) as usize, (cy + r_px).min(g.h as f64 - 1.0) as usize);
    for y in (y0..=y1).step_by(2) {
        for x in (x0..=x1).step_by(2) {
            if (x as f64 - cx).powi(2) + (y as f64 - cy).powi(2) > r2 {
                continue;
            }
            total += 1;
            let e = g.data[y * g.w + x];
            if e > ref_e + 1.0 {
                above += 1;
            } else if e < ref_e - 1.0 {
                below += 1;
            }
        }
    }
    let t = total.max(1) as f64;
    (above as f64 / t, below as f64 / t)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cone() -> Grid {
        let (w, h) = (101, 101);
        let mut data = vec![0f32; w * h];
        for y in 0..h {
            for x in 0..w {
                let d = ((x as f32 - 50.0).hypot(y as f32 - 50.0)) * 30.0;
                data[y * w + x] = (1000.0 - d).max(0.0);
            }
        }
        Grid {
            w,
            h,
            z: 12,
            px0: 0.0,
            py0: 0.0,
            mpp: 30.0,
            data,
        }
    }

    #[test]
    fn cone_hides_far_side() {
        let g = cone();
        let v = viewshed(&g, 10.0, 50.0, 1.7, 0.0);
        assert!(v.mask[50 * 101 + 20] > 0);
        assert_eq!(v.mask[50 * 101 + 90], 0);
        assert!(line_of_sight(&g, (10.0, 50.0), (90.0, 50.0), 1.7).is_some());
        assert!(line_of_sight(&g, (10.0, 50.0), (10.0, 90.0), 1.7).is_none());
    }

    #[test]
    fn low_sun_casts_shadow_behind_cone() {
        let g = cone();
        // Słońce nisko na wschodzie: cień na zachód od szczytu
        let s = shadows(&g, [0.97, 0.24, 0.0]);
        assert!(s[50 * 101 + 10] < 128, "{}", s[50 * 101 + 10]);
        assert!(s[50 * 101 + 80] > 128);
    }
}
