//! Sieć szlaków złożona z sektorów OSM: rysowanie na terenie i wybieranie klikiem.
//! Zostają tylko odcinki przebiegające przez Polskę.

use std::collections::HashMap;

use crate::geo::haversine;
use crate::metrics::{climb, TrailColor};
use crate::osm::{RouteInfo, SectorData};

pub struct Edge {
    pub a: u32,
    pub b: u32,
    pub lon: Vec<f64>,
    pub lat: Vec<f64>,
    pub x: Vec<f32>,
    pub z: Vec<f32>,
    pub e: Vec<f32>,
    pub len2: f64,
    pub len3: f64,
    pub up: f64,
    pub down: f64,
    pub colors: Vec<TrailColor>,
    /// Indeksy w `TrailGraph::routes`.
    pub routes: Vec<u32>,
    pub sac: u8,
    pub oneway: i8,
    pub name: String,
    pub way: i64,
}

impl Edge {
    pub fn n(&self) -> usize {
        self.lon.len()
    }
}

pub struct GNode {
    pub lon: f64,
    pub lat: f64,
    pub edges: Vec<u32>,
}

#[derive(Clone, Copy, Debug)]
pub struct Snap {
    pub edge: u32,
    /// Pozycja wzdłuż krawędzi w jednostkach indeksu (i + t).
    pub p: f64,
    pub lon: f64,
    pub lat: f64,
    pub e: f32,
    pub dist: f64,
}


/// Lokalny układ metrów do indeksu przestrzennego.
#[derive(Clone, Copy)]
struct Local {
    lon0: f64,
    lat0: f64,
    kx: f64,
}

impl Local {
    fn xz(&self, lon: f64, lat: f64) -> (f32, f32) {
        (
            ((lon - self.lon0) * self.kx) as f32,
            (-(lat - self.lat0) * 110_574.0) as f32,
        )
    }
}

const CELL: f32 = 250.0;

pub struct TrailGraph {
    pub nodes: Vec<GNode>,
    pub edges: Vec<Edge>,
    pub routes: Vec<RouteInfo>,
    index: HashMap<(i32, i32), Vec<(u32, u32)>>,
    local: Local,
    pub total_km: f64,
}

impl TrailGraph {
    pub fn empty() -> Self {
        Self {
            nodes: Vec::new(),
            edges: Vec::new(),
            routes: Vec::new(),
            index: HashMap::new(),
            local: Local {
                lon0: 0.0,
                lat0: 0.0,
                kx: 111_320.0,
            },
            total_km: 0.0,
        }
    }

    /// Złożenie grafu z sektorów; `height` próbkuje wysokość (zagęszczenie co ~10 m).
    pub fn build(sectors: &[&SectorData], height: &(dyn Fn(f64, f64) -> f32 + Sync)) -> Self {
        let mut g = Self::empty();
        let Some(first) = sectors.iter().find(|s| !s.nodes.is_empty()) else {
            return g;
        };
        let (lon0, lat0) = (first.nodes[0].1, first.nodes[0].2);
        g.local = Local {
            lon0,
            lat0,
            kx: 111_320.0 * lat0.to_radians().cos(),
        };
        // relacje (po identyfikatorze) i węzły (po identyfikatorze OSM)
        let mut route_idx: HashMap<i64, u32> = HashMap::new();
        let mut coords: HashMap<i64, (f64, f64)> = HashMap::new();
        struct Run<'a> {
            ids: Vec<i64>,
            routes: Vec<u32>,
            meta: &'a crate::osm::WayRun,
        }
        let mut runs: Vec<Run> = Vec::new();
        for s in sectors {
            let rmap: Vec<u32> = s
                .routes
                .iter()
                .map(|r| {
                    *route_idx.entry(r.id).or_insert_with(|| {
                        g.routes.push(r.clone());
                        (g.routes.len() - 1) as u32
                    })
                })
                .collect();
            for &(id, lon, lat) in &s.nodes {
                coords.insert(id, (lon, lat));
            }
            for r in &s.runs {
                runs.push(Run {
                    ids: r.nodes.iter().map(|&i| s.nodes[i as usize].0).collect(),
                    routes: r.routes.iter().map(|&i| rmap[i as usize]).collect(),
                    meta: r,
                });
            }
        }
        // węzły grafu: końce ciągów i punkty wspólne wielu dróg
        let mut uses: HashMap<i64, u32> = HashMap::new();
        for r in &runs {
            for id in &r.ids {
                *uses.entry(*id).or_default() += 1;
            }
            *uses.entry(r.ids[0]).or_default() += 2;
            *uses.entry(*r.ids.last().unwrap()).or_default() += 2;
        }
        let mut gidx: HashMap<i64, u32> = HashMap::new();
        let mut node_of = |g: &mut TrailGraph, id: i64| -> u32 {
            *gidx.entry(id).or_insert_with(|| {
                let (lon, lat) = coords[&id];
                g.nodes.push(GNode {
                    lon,
                    lat,
                    edges: Vec::new(),
                });
                (g.nodes.len() - 1) as u32
            })
        };
        struct Raw {
            a: u32,
            b: u32,
            pts: Vec<(f64, f64)>,
            run: usize,
        }
        let mut raws = Vec::new();
        let mut seen = std::collections::HashSet::new();
        for (ri, r) in runs.iter().enumerate() {
            let mut start = 0;
            for i in 1..r.ids.len() {
                if uses[&r.ids[i]] > 1 || i == r.ids.len() - 1 {
                    let seg = &r.ids[start..=i];
                    start = i;
                    let key: Vec<i64> = if seg[0] < seg[seg.len() - 1] {
                        seg.to_vec()
                    } else {
                        seg.iter().rev().copied().collect()
                    };
                    if !seen.insert(key) {
                        continue;
                    }
                    let a = node_of(&mut g, seg[0]);
                    let b = node_of(&mut g, seg[seg.len() - 1]);
                    // zagęszczenie geometrii co ~10 m
                    let mut pts = Vec::new();
                    for k in 0..seg.len() {
                        let c = coords[&seg[k]];
                        if k > 0 {
                            let p = coords[&seg[k - 1]];
                            let d = haversine(p.0, p.1, c.0, c.1);
                            let m = (d / 10.0).floor() as usize;
                            for j in 1..=m {
                                let f = j as f64 / (m + 1) as f64;
                                pts.push((p.0 + (c.0 - p.0) * f, p.1 + (c.1 - p.1) * f));
                            }
                        }
                        pts.push(c);
                    }
                    if pts.iter().any(|p| crate::national::in_poland(p.0, p.1)) {
                        raws.push(Raw { a, b, pts, run: ri });
                    }
                }
            }
        }
        // wysokości równolegle (próbkowanie może czytać dysk albo pobierać kafle)
        use rayon::prelude::*;
        let heights: Vec<Vec<f32>> = raws
            .par_iter()
            .map(|r| r.pts.iter().map(|&(lon, lat)| height(lon, lat)).collect())
            .collect();
        let mut total = 0.0;
        for (raw, e) in raws.into_iter().zip(heights) {
            let r = &runs[raw.run];
            let n = raw.pts.len();
            let mut len2 = 0.0;
            let mut len3 = 0.0;
            let (mut x, mut z) = (Vec::with_capacity(n), Vec::with_capacity(n));
            for i in 0..n {
                let (xx, zz) = g.local.xz(raw.pts[i].0, raw.pts[i].1);
                x.push(xx);
                z.push(zz);
                if i > 0 {
                    let h = haversine(raw.pts[i - 1].0, raw.pts[i - 1].1, raw.pts[i].0, raw.pts[i].1);
                    len2 += h;
                    len3 += h.hypot((e[i] - e[i - 1]) as f64);
                }
            }
            let (up, down) = climb(&e);
            total += len2;
            let mut colors: Vec<TrailColor> = r
                .routes
                .iter()
                .map(|&ri| TrailColor::parse(&g.routes[ri as usize].color))
                .filter(|c| *c != TrailColor::Other)
                .collect();
            colors.sort();
            colors.dedup();
            if colors.is_empty() {
                colors.push(TrailColor::Other);
            }
            let id = g.edges.len() as u32;
            g.edges.push(Edge {
                a: raw.a,
                b: raw.b,
                lon: raw.pts.iter().map(|p| p.0).collect(),
                lat: raw.pts.iter().map(|p| p.1).collect(),
                x,
                z,
                e,
                len2,
                len3,
                up,
                down,
                colors,
                routes: r.routes.clone(),
                sac: r.meta.sac,
                oneway: r.meta.oneway,
                name: r.meta.name.clone(),
                way: r.meta.way,
            });
            g.nodes[raw.a as usize].edges.push(id);
            if raw.b != raw.a {
                g.nodes[raw.b as usize].edges.push(id);
            }
        }
        for (id, ed) in g.edges.iter().enumerate() {
            for i in 0..ed.n() - 1 {
                let k0 = cell(ed.x[i], ed.z[i]);
                let k1 = cell(ed.x[i + 1], ed.z[i + 1]);
                g.index.entry(k0).or_default().push((id as u32, i as u32));
                if k1 != k0 {
                    g.index.entry(k1).or_default().push((id as u32, i as u32));
                }
            }
        }
        g.total_km = total / 1000.0;
        g
    }

    /// Najbliższy punkt na sieci szlaków (w promieniu `max_dist` m).
    pub fn snap(&self, lon: f64, lat: f64, max_dist: f64) -> Option<Snap> {
        if self.edges.is_empty() {
            return None;
        }
        let (x, z) = self.local.xz(lon, lat);
        let r = (max_dist as f32 / CELL).ceil() as i32;
        let (cx, cz) = cell(x, z);
        let mut best: Option<Snap> = None;
        let mut best_d = max_dist;
        for i in -r..=r {
            for j in -r..=r {
                let Some(list) = self.index.get(&(cx + i, cz + j)) else {
                    continue;
                };
                for &(eid, s) in list {
                    let ed = &self.edges[eid as usize];
                    let s = s as usize;
                    let (ax, az, bx, bz) = (ed.x[s], ed.z[s], ed.x[s + 1], ed.z[s + 1]);
                    let (dx, dz) = (bx - ax, bz - az);
                    let l2 = dx * dx + dz * dz;
                    let t = if l2 > 0.0 {
                        (((x - ax) * dx + (z - az) * dz) / l2).clamp(0.0, 1.0)
                    } else {
                        0.0
                    };
                    let d = ((ax + dx * t - x).hypot(az + dz * t - z)) as f64;
                    if d < best_d {
                        best_d = d;
                        let tt = t as f64;
                        best = Some(Snap {
                            edge: eid,
                            p: s as f64 + tt,
                            lon: ed.lon[s] + (ed.lon[s + 1] - ed.lon[s]) * tt,
                            lat: ed.lat[s] + (ed.lat[s + 1] - ed.lat[s]) * tt,
                            e: ed.e[s] + (ed.e[s + 1] - ed.e[s]) * t,
                            dist: d,
                        });
                    }
                }
            }
        }
        best
    }
}

fn cell(x: f32, z: f32) -> (i32, i32) {
    ((x / CELL).floor() as i32, (z / CELL).floor() as i32)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::osm::{SectorData, WayRun};

    fn sector(lon: f64, lat: f64) -> SectorData {
        let run = WayRun {
            way: 10,
            nodes: vec![0, 1],
            routes: vec![],
            sac: 1,
            oneway: 0,
            name: String::new(),
            highway: "path".into(),
        };
        SectorData {
            format: 2,
            fetched: 0,
            nodes: vec![(1, lon, lat), (2, lon + 0.0146, lat)],
            runs: vec![run],
            routes: vec![],
            pois: vec![],
        }
    }

    #[test]
    fn snaps_to_trail_in_poland() {
        let s = sector(20.0, 52.0);
        let g = TrailGraph::build(&[&s], &|_, _| 100.0);
        assert_eq!(g.edges.len(), 1);
        assert!((g.total_km - 1.0).abs() < 0.05, "{}", g.total_km);
        assert!(g.snap(20.007, 52.0001, 100.0).is_some());
        assert!(g.snap(20.5, 52.5, 100.0).is_none());
    }

    #[test]
    fn drops_trails_outside_poland() {
        let s = sector(30.0, 52.0);
        let g = TrailGraph::build(&[&s], &|_, _| 100.0);
        assert!(g.edges.is_empty());
    }
}
