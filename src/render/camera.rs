//! Kamera orbitalna nad terenem i lokalna ramka świata.
//!
//! Ramka: początek w punkcie Web Mercator (`ox`, `oy`), osie X – wschód, Y – góra, Z – południe,
//! jednostka – metr na szerokości początku. Współrzędne względem ramki trzymają precyzję `f32`
//! w buforach GPU; gdy kamera odleci daleko, ramka przesuwa się razem z nią.

use glam::{DMat4, DVec3, Mat4, Vec3};

use crate::geo::{lat_to_y, lon_to_x, world_m, x_to_lon, y_to_lat, EARTH_R};

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Frame {
    pub ox: f64,
    pub oy: f64,
    /// Metry na jednostkę świata (0..1) w początku ramki.
    pub m: f64,
}

impl Frame {
    pub fn at(lon: f64, lat: f64) -> Frame {
        Frame {
            ox: lon_to_x(lon),
            oy: lat_to_y(lat),
            m: world_m(lat),
        }
    }

    pub fn to_local(&self, lon: f64, lat: f64) -> (f64, f64) {
        ((lon_to_x(lon) - self.ox) * self.m, (lat_to_y(lat) - self.oy) * self.m)
    }

    pub fn to_lonlat(&self, x: f64, z: f64) -> (f64, f64) {
        (x_to_lon(x / self.m + self.ox), y_to_lat(z / self.m + self.oy))
    }

    /// Współrzędne świata (0..1) → lokalne metry.
    pub fn world_to_local(&self, wx: f64, wy: f64) -> (f64, f64) {
        ((wx - self.ox) * self.m, (wy - self.oy) * self.m)
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Camera {
    pub lon: f64,
    pub lat: f64,
    /// Wysokość punktu, wokół którego obraca się kamera (m n.p.m., bez przewyższenia).
    pub height: f64,
    /// Kierunek patrzenia (0 = północ, stopnie).
    pub heading: f64,
    /// Nachylenie w dół od poziomu (stopnie).
    pub pitch: f64,
    /// Odległość kamery od punktu (m).
    pub distance: f64,
    pub fov_y: f64,
}

impl Default for Camera {
    fn default() -> Self {
        Camera {
            lon: 19.2,
            lat: 52.0,
            height: 150.0,
            heading: 0.0,
            pitch: 55.0,
            distance: 700_000.0,
            fov_y: 45.0,
        }
    }
}

/// Wynik przygotowania kamery dla klatki.
#[derive(Clone, Copy, Debug)]
pub struct View {
    pub frame: Frame,
    /// Położenie oka w ramce (y z przewyższeniem).
    pub eye: DVec3,
    pub target: DVec3,
    pub view: DMat4,
    pub proj: DMat4,
    pub view_proj: DMat4,
    pub exag: f64,
    pub size: [f32; 2],
    pub near: f64,
}

impl Camera {
    pub fn view(&self, frame: Frame, exag: f64, size: [f32; 2], ground_below_eye: f64) -> View {
        let (tx, tz) = frame.to_local(self.lon, self.lat);
        let target = DVec3::new(tx, self.height * exag, tz);
        let (h, p) = (self.heading.to_radians(), self.pitch.to_radians());
        let dir_h = DVec3::new(h.sin(), 0.0, -h.cos());
        let eye = target - dir_h * self.distance * p.cos() + DVec3::Y * self.distance * p.sin();
        let view = DMat4::look_at_rh(eye, target, DVec3::Y);
        let above = (eye.y - ground_below_eye * exag).max(1.0);
        let near = (above * 0.05).clamp(0.3, 200.0);
        let aspect = (size[0] / size[1].max(1.0)) as f64;
        let proj = DMat4::perspective_infinite_reverse_rh(self.fov_y.to_radians(), aspect, near);
        View {
            frame,
            eye,
            target,
            view,
            proj,
            view_proj: proj * view,
            exag,
            size,
            near,
        }
    }
}

impl View {
    /// Macierz widoku-projekcji względem oka (dla GPU: pozycje podawane względem kamery).
    pub fn view_proj_rel(&self) -> Mat4 {
        let v = DMat4::look_at_rh(DVec3::ZERO, self.target - self.eye, DVec3::Y);
        (self.proj * v).as_mat4()
    }

    pub fn eye_f32(&self) -> Vec3 {
        self.eye.as_vec3()
    }

    /// Punkt lokalny → piksele ekranu (logiczne, względem lewego górnego rogu widoku); `None` za kamerą.
    pub fn project(&self, p: DVec3) -> Option<[f32; 2]> {
        let d = p - self.eye;
        // krzywizna Ziemi jak w shaderze
        let hd = (d.x * d.x + d.z * d.z).sqrt();
        let p = DVec3::new(p.x, p.y - hd * hd / (2.0 * EARTH_R) * 0.87, p.z);
        let c = self.view_proj * p.extend(1.0);
        if c.w <= 1e-6 {
            return None;
        }
        let n = c.truncate() / c.w;
        if n.z < 0.0 || n.z > 1.0 {
            return None;
        }
        Some([
            ((n.x + 1.0) * 0.5 * self.size[0] as f64) as f32,
            ((1.0 - n.y) * 0.5 * self.size[1] as f64) as f32,
        ])
    }

    /// Promień z oka przez piksel ekranu (kierunek znormalizowany).
    pub fn ray(&self, px: [f32; 2]) -> DVec3 {
        let nx = px[0] as f64 / self.size[0] as f64 * 2.0 - 1.0;
        let ny = 1.0 - px[1] as f64 / self.size[1] as f64 * 2.0;
        let inv = self.view_proj.inverse();
        let a = inv.project_point3(DVec3::new(nx, ny, 1.0));
        let b = inv.project_point3(DVec3::new(nx, ny, 0.01));
        (b - a).normalize()
    }

    /// Przecięcie promienia z terenem; `height(x, z)` – wysokość rzeczywista w punkcie ramki.
    /// Zwraca punkt (x, wysokość rzeczywista, z).
    pub fn raycast(&self, dir: DVec3, height: &dyn Fn(f64, f64) -> Option<f64>) -> Option<DVec3> {
        let o = self.eye;
        let exag = self.exag;
        let mut t = 0.0f64;
        if dir.y >= 0.0 && o.y > 9000.0 * exag {
            return None;
        }
        // start na płaszczyźnie najwyższych szczytów
        let top = 9000.0 * exag;
        if o.y > top {
            t = (o.y - top) / -dir.y;
        }
        let max_t = t + 600_000.0;
        let mut prev_t = t;
        let curv = |t: f64| {
            let hd = (dir.x * dir.x + dir.z * dir.z).sqrt() * t;
            hd * hd / (2.0 * EARTH_R) * 0.87
        };
        let gap_at = |t: f64| -> Option<f64> {
            let p = o + dir * t;
            height(p.x, p.z).map(|h| p.y + curv(t) - h * exag)
        };
        let min_step = (self.near * 0.5).max(0.5);
        for _ in 0..1500 {
            if t > max_t {
                return None;
            }
            let gap = match gap_at(t) {
                Some(g) => g,
                None => {
                    t += (t * 0.02).max(200.0);
                    prev_t = t;
                    continue;
                }
            };
            if gap <= 0.0 {
                let (mut lo, mut hi) = (prev_t, t);
                for _ in 0..24 {
                    let m = (lo + hi) / 2.0;
                    if gap_at(m).is_some_and(|g| g > 0.0) {
                        lo = m
                    } else {
                        hi = m
                    }
                }
                let p = o + dir * hi;
                let h = height(p.x, p.z)?;
                return Some(DVec3::new(p.x, h, p.z));
            }
            prev_t = t;
            t += (gap * 0.5).max(min_step).max(t * 0.0015);
        }
        None
    }
}
