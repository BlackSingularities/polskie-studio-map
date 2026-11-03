// Polskie Studio Map – teren kaflowy: wysokości z kafla DEM (257×257, narożniki pikseli), gradient do cieniowania,
// style (teren, hipsometria, nachylenie, ekspozycja, satelita/ortofoto, mapa), mapa normalnych, cienie, soczewki, mgła.

struct Globals {
    view_proj: mat4x4<f32>,
    eye: vec4<f32>,          // oko w ramce, w = przewyższenie
    sun_dir: vec4<f32>,      // kierunek do Słońca, w = natężenie (0 nocą)
    sun_color: vec4<f32>,
    sky_color: vec4<f32>,
    fog_color: vec4<f32>,    // w = gęstość mgły
    style: vec4<f32>,        // styl, cięcie poziomic, poziomice, siatka km
    season: vec4<f32>,       // granica śniegu, śnieg, jesień, szum
    lens: vec4<f32>,         // x, z, wysokość, tryb (0 brak, 1 poziomica, 2 pas, 3 względna, 4 widoczność)
    lens2: vec4<f32>,        // pas ±m, zakres względny, przypięta, czas
    cursor: vec4<f32>,       // x, z, wysokość, aktywny
    shadow_rect: vec4<f32>,  // x0, z0, 1/szer, 1/wys (m ramki)
    vs_rect: vec4<f32>,
    flags: vec4<f32>,        // cienie, widoczność, pokrycie terenu, mapa normalnych
    biome: vec4<f32>,        // las, kosodrzewina, skały, okluzja
    grid_origin: vec4<f32>,  // przesunięcie siatki km (m), metry na m ramki
    land_rect: vec4<f32>,    // maska pokrycia terenu: x0, z0, 1/szer., 1/wys.
};

struct Tile {
    rect: vec4<f32>,  // x0, z0, rozmiar (m), głębokość fartucha
    dem: vec4<f32>,   // przesunięcie u, v, skala w kaflu DEM, -
    img: vec4<f32>,   // przesunięcie u, v, skala w kaflu obrazu, obraz jest (1/0)
    info: vec4<f32>,  // poziom, -, -, -
};

@group(0) @binding(0) var<uniform> g: Globals;
@group(0) @binding(1) var samp: sampler;
@group(0) @binding(2) var shadow_tex: texture_2d<f32>;
@group(0) @binding(3) var vs_tex: texture_2d<f32>;
@group(0) @binding(4) var land_tex: texture_2d<f32>;
@group(0) @binding(5) var water_tex: texture_2d<f32>;
@group(1) @binding(0) var<uniform> tile: Tile;
@group(2) @binding(0) var height_tex: texture_2d<f32>;
@group(2) @binding(1) var grad_tex: texture_2d<f32>;
@group(3) @binding(0) var img_tex: texture_2d<f32>;

const EARTH_R: f32 = 6371008.8;

fn height_at(uv: vec2<f32>) -> f32 {
    let p = clamp(uv, vec2(0.0), vec2(1.0)) * 256.0;
    let i = min(vec2<i32>(floor(p)), vec2(255));
    let f = p - vec2<f32>(i);
    let a = textureLoad(height_tex, i, 0).r;
    let b = textureLoad(height_tex, i + vec2(1, 0), 0).r;
    let c = textureLoad(height_tex, i + vec2(0, 1), 0).r;
    let d = textureLoad(height_tex, i + vec2(1, 1), 0).r;
    return mix(mix(a, b, f.x), mix(c, d, f.x), f.y);
}

// poziom lustra wody (OSM + wysokość brzegu) w punkcie ramki; poniżej -1000 = brak wody
fn water_level(wp: vec2<f32>) -> f32 {
    let uv = (wp - g.land_rect.xy) * g.land_rect.zw;
    if (g.flags.z < 0.5 || any(uv < vec2(0.0)) || any(uv >= vec2(1.0))) {
        return -10000.0;
    }
    let dim = vec2<f32>(textureDimensions(water_tex));
    return textureLoad(water_tex, vec2<i32>(uv * dim), 0).r;
}

struct VOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) wp: vec2<f32>,
    @location(1) dem_uv: vec2<f32>,
    @location(2) img_uv: vec2<f32>,
    @location(3) dist: f32,
    @location(4) h: f32,
};

@vertex
fn vs_main(@location(0) uv: vec2<f32>, @location(1) skirt: f32) -> VOut {
    let duv = tile.dem.xy + uv * tile.dem.z;
    var h = height_at(duv);
    let exag = g.eye.w;
    // tafla jeziora płasko na wysokości brzegu (model terenu ma tam dno albo szum)
    let wl = water_level(vec2(tile.rect.x + uv.x * tile.rect.z, tile.rect.y + uv.y * tile.rect.z));
    if (wl > -1000.0) {
        h = wl;
    }
    let world = vec3(tile.rect.x + uv.x * tile.rect.z, h * exag - skirt * tile.rect.w, tile.rect.y + uv.y * tile.rect.z);
    var rel = world - g.eye.xyz;
    let hd2 = dot(rel.xz, rel.xz);
    rel.y -= hd2 / (2.0 * EARTH_R) * 0.87;
    var o: VOut;
    o.pos = g.view_proj * vec4(rel, 1.0);
    o.wp = world.xz;
    o.dem_uv = duv;
    o.img_uv = tile.img.xy + uv * tile.img.z;
    o.dist = length(rel);
    o.h = h;
    return o;
}

fn srgb(c: vec3<f32>) -> vec3<f32> { return pow(c, vec3(2.2)); }

fn hash(p: vec2<f32>) -> f32 {
    var p3 = fract(vec3(p.x, p.y, p.x) * 0.1031);
    p3 += dot(p3, p3.yzx + 33.33);
    return fract((p3.x + p3.y) * p3.z);
}

fn vnoise(p: vec2<f32>) -> f32 {
    let i = floor(p);
    let f = fract(p);
    let u = f * f * (3.0 - 2.0 * f);
    return mix(mix(hash(i), hash(i + vec2(1.0, 0.0)), u.x), mix(hash(i + vec2(0.0, 1.0)), hash(i + vec2(1.0, 1.0)), u.x), u.y);
}

fn fbm(p: vec2<f32>) -> f32 {
    return vnoise(p) * 0.55 + vnoise(p * 2.03 + 7.1) * 0.3 + vnoise(p * 4.1 + 3.3) * 0.15;
}

fn hsv2rgb(c: vec3<f32>) -> vec3<f32> {
    let p = abs(fract(c.xxx + vec3(0.0, 2.0 / 3.0, 1.0 / 3.0)) * 6.0 - 3.0);
    return c.z * mix(vec3(1.0), clamp(p - 1.0, vec3(0.0), vec3(1.0)), c.y);
}

fn hypso(e: f32) -> vec3<f32> {
    var stops = array<vec4<f32>, 10>(
        vec4(0.0, 0.40, 0.62, 0.40), vec4(300.0, 0.55, 0.73, 0.45), vec4(600.0, 0.76, 0.82, 0.52),
        vec4(900.0, 0.93, 0.89, 0.60), vec4(1200.0, 0.94, 0.79, 0.52), vec4(1500.0, 0.87, 0.64, 0.41),
        vec4(1800.0, 0.74, 0.49, 0.32), vec4(2100.0, 0.60, 0.40, 0.31), vec4(2400.0, 0.66, 0.60, 0.60),
        vec4(2800.0, 0.97, 0.97, 0.98));
    var c = stops[0].yzw;
    for (var i = 1; i < 10; i++) {
        let a = stops[i - 1];
        let b = stops[i];
        if (e >= a.x) {
            c = mix(a.yzw, b.yzw, clamp((e - a.x) / (b.x - a.x), 0.0, 1.0));
        }
    }
    return srgb(c);
}

fn in_rect(wp: vec2<f32>, r: vec4<f32>) -> vec3<f32> {
    let uv = (wp - r.xy) * r.zw;
    let inside = select(0.0, 1.0, all(uv >= vec2(0.0)) && all(uv <= vec2(1.0)));
    return vec3(uv, inside);
}

@fragment
fn fs_main(in: VOut) -> @location(0) vec4<f32> {
    let exag = g.eye.w;
    let style = i32(g.style.x + 0.5);
    let wl = water_level(in.wp);
    let on_water = wl > -1000.0;
    let h = select(height_at(in.dem_uv), wl, on_water);
    let grad = select(textureSample(grad_tex, samp, in.dem_uv).xy, vec2(0.0), on_water);
    // lekka mapa normalnych z bliska: na zdjęciach z jasności obrazu, inaczej z drobnego szumu
    let px = vec2(1.5) / vec2<f32>(textureDimensions(img_tex));
    let lum = vec3(0.3, 0.59, 0.11);
    let lx = dot(textureSample(img_tex, samp, in.img_uv + vec2(px.x, 0.0)).rgb - textureSample(img_tex, samp, in.img_uv - vec2(px.x, 0.0)).rgb, lum);
    let lz = dot(textureSample(img_tex, samp, in.img_uv + vec2(0.0, px.y)).rgb - textureSample(img_tex, samp, in.img_uv - vec2(0.0, px.y)).rgb, lum);
    let mx = fbm((in.wp + vec2(2.0, 0.0)) / 11.0) - fbm((in.wp - vec2(2.0, 0.0)) / 11.0);
    let mz = fbm((in.wp + vec2(0.0, 2.0)) / 11.0) - fbm((in.wp - vec2(0.0, 2.0)) / 11.0);
    let detail_on = g.flags.w * (1.0 - smoothstep(1500.0, 25000.0, in.dist));
    let photo = style == 4 && tile.img.w > 0.5;
    let detail = select(vec2(mx, mz) * 0.8, vec2(lx, lz) * 1.6, photo) * detail_on;
    let n = normalize(vec3(-grad.x * exag - detail.x, 1.0, -grad.y * exag - detail.y));
    let slope = length(grad);
    let slope_deg = degrees(atan(slope));
    // ekspozycja: kierunek spadku (0 = N)
    let aspect = (degrees(atan2(-grad.x, grad.y)) + 360.0) % 360.0;
    let north = cos(radians(aspect));
    let wp = in.wp;
    let sun = g.sun_dir.xyz;

    // cień rzucany (mapa z analizy) i światło
    let sr = in_rect(wp, g.shadow_rect);
    let shadow_s = textureSample(shadow_tex, samp, clamp(sr.xy, vec2(0.0), vec2(1.0))).r;
    let vr = in_rect(wp, g.vs_rect);
    let vs_s = textureSample(vs_tex, samp, clamp(vr.xy, vec2(0.0), vec2(1.0))).r;
    let img_s = textureSample(img_tex, samp, in.img_uv).rgb;
    let lr = in_rect(wp, g.land_rect);
    let land_s = textureSample(land_tex, samp, clamp(lr.xy, vec2(0.0), vec2(1.0)));
    // pokrycie z OSM: R woda, G las, B skały (1) / zarośla (~0,43)
    let land = select(vec4(0.0), land_s, g.flags.z > 0.5 && lr.z > 0.5 && land_s.a > 0.5);
    let has_land = g.flags.z > 0.5 && lr.z > 0.5 && land_s.a > 0.5;
    var shadow = 1.0;
    if (g.flags.x > 0.5 && sr.z > 0.5) {
        shadow = shadow_s;
    }
    let ndl = max(dot(n, sun), 0.0);
    let sky_amb = mix(0.55, 1.0, n.y * 0.5 + 0.5);
    let light = g.sun_color.rgb * ndl * shadow * g.sun_dir.w + g.sky_color.rgb * sky_amb * 0.42;

    var base = vec3(0.5);
    var lit = true;
    let snowline = g.season.x;
    let autumn = g.season.z;
    let noise_amt = g.season.w;
    var water = 0.0;

    if (style == 0 || style == 4) {
        // piętra roślinności zależne od wysokości, nachylenia i ekspozycji
        let jitter = (fbm(wp / 180.0) - 0.5) * 140.0 + north * 60.0;
        let e = h + jitter;
        let meadow_low = mix(srgb(vec3(0.44, 0.52, 0.26)), srgb(vec3(0.58, 0.58, 0.34)), vnoise(wp / 260.0));
        let beech = mix(srgb(vec3(0.20, 0.29, 0.12)), srgb(vec3(0.55, 0.33, 0.12)), autumn * smoothstep(0.35, 0.7, vnoise(wp / 90.0)));
        let spruce = srgb(vec3(0.10, 0.17, 0.10)) * (0.8 + 0.4 * vnoise(wp / 9.0));
        let pine = mix(srgb(vec3(0.17, 0.25, 0.11)), srgb(vec3(0.24, 0.31, 0.14)), vnoise(wp / 12.0));
        var alpine = mix(srgb(vec3(0.46, 0.47, 0.28)), srgb(vec3(0.58, 0.45, 0.26)), autumn);
        alpine = mix(alpine, srgb(vec3(0.52, 0.52, 0.40)), vnoise(wp / 40.0) * 0.5);
        var rock = mix(srgb(vec3(0.44, 0.43, 0.41)), srgb(vec3(0.58, 0.56, 0.52)), fbm(wp / 25.0));
        rock = mix(rock, srgb(vec3(0.36, 0.33, 0.30)), smoothstep(0.9, 1.2, slope) * 0.6);
        let forest_line = g.biome.x;
        let shrub_line = g.biome.y;
        let rock_line = g.biome.z;
        // las: regiel dolny (buk) → górny (świerk)
        let forest_mix = smoothstep(forest_line * 0.62, forest_line * 0.78, e);
        var c = mix(beech, spruce, forest_mix);
        // polany i hale w dolinach płaskich
        let clearing = smoothstep(0.55, 0.75, vnoise(wp / 420.0)) * (1.0 - smoothstep(0.15, 0.35, slope));
        c = mix(c, meadow_low, clearing * (1.0 - smoothstep(forest_line - 80.0, forest_line, e)) * 0.8);
        // niziny poniżej regla: łąki i pola
        c = mix(meadow_low, c, smoothstep(450.0, 650.0, e) * 0.7 + 0.3 * smoothstep(0.1, 0.3, slope));
        // kosodrzewina
        c = mix(c, pine, smoothstep(forest_line - 40.0, forest_line + 40.0, e));
        // hale
        c = mix(c, alpine, smoothstep(shrub_line - 40.0, shrub_line + 60.0, e));
        // turnie i piargi: wysoko albo stromo
        let rock_mask = max(smoothstep(rock_line - 60.0, rock_line + 160.0, e) * smoothstep(0.25, 0.6, slope + 0.2),
                            smoothstep(0.95, 1.4, slope));
        c = mix(c, rock, clamp(rock_mask, 0.0, 1.0));
        if (has_land) {
            // rzeczywiste lasy, kosodrzewina, piargi i wody z OSM zamiast przybliżenia z wysokości
            let below = 1.0 - smoothstep(forest_line - 60.0, forest_line + 60.0, e);
            let forest_c = mix(beech, spruce, forest_mix);
            c = mix(c, mix(meadow_low, forest_c, land.g), below * 0.85);
            let scrub = smoothstep(0.3, 0.5, land.b) * (1.0 - smoothstep(0.7, 0.9, land.b));
            c = mix(c, pine, scrub * 0.9);
            c = mix(c, rock, smoothstep(0.8, 1.0, land.b) * 0.9);
            if (land.r > 0.5 && slope < 0.15) {
                water = 1.0;
            }
        }
        if (on_water) {
            water = 1.0;
        }
        // śnieg: od granicy (dłużej na północnych stokach i w żlebach), nie na ścianach
        if (g.season.y > 0.5) {
            let sline = snowline - north * 180.0 + (fbm(wp / 60.0) - 0.5) * 160.0;
            let snow = smoothstep(sline - 40.0, sline + 60.0, h) * (1.0 - smoothstep(0.9, 1.35, slope));
            c = mix(c, srgb(vec3(0.93, 0.95, 0.98)), snow);
        }
        base = c;
        if (style == 4 && tile.img.w > 0.5) {
            // zdjęcie ma już światło i cienie z chwili wykonania – słabsze cieniowanie
            // Sentinel-2 jest ciemniejszy od ortofoto (img.w = 2), a kafle obu leżą obok siebie
            base = img_s * select(1.05, 1.45, tile.img.w > 1.5);
        }
        // woda: płaskie miejsca na niskich wysokościach (morza, jeziora z modelu)
        if (h < 0.5 && slope < 0.01) {
            water = 1.0;
        }
    } else if (style == 1) {
        base = hypso(h);
    } else if (style == 2) {
        var c = srgb(vec3(0.94, 0.93, 0.89));
        if (slope_deg >= 45.0) { c = srgb(vec3(0.33, 0.18, 0.46)); }
        else if (slope_deg >= 40.0) { c = srgb(vec3(0.63, 0.20, 0.55)); }
        else if (slope_deg >= 35.0) { c = srgb(vec3(0.89, 0.18, 0.15)); }
        else if (slope_deg >= 30.0) { c = srgb(vec3(0.98, 0.56, 0.12)); }
        else if (slope_deg >= 25.0) { c = srgb(vec3(0.99, 0.87, 0.30)); }
        else if (slope_deg >= 15.0) { c = srgb(vec3(0.84, 0.90, 0.72)); }
        base = c;
    } else if (style == 3) {
        let sat = clamp(slope * 2.5, 0.0, 0.85);
        base = srgb(hsv2rgb(vec3(aspect / 360.0, sat, 0.93)));
    } else {
        // mapa turystyczna: papier, lasy i skały delikatnie
        let paper = srgb(vec3(0.97, 0.95, 0.90));
        let wood = srgb(vec3(0.80, 0.89, 0.70));
        var c = mix(wood, paper, smoothstep(g.biome.x - 60.0, g.biome.x + 60.0, h));
        if (has_land) {
            c = mix(paper, wood, land.g);
            if (land.r > 0.5 && slope < 0.15) {
                water = 1.0;
            }
        }
        if (on_water) {
            water = 1.0;
        }
        let rk = smoothstep(0.9, 1.3, slope) + smoothstep(g.biome.z, g.biome.z + 250.0, h) * smoothstep(0.4, 0.8, slope);
        c = mix(c, srgb(vec3(0.80, 0.79, 0.78)), clamp(rk, 0.0, 1.0));
        base = c;
    }

    var col: vec3<f32>;
    if (style == 4 && tile.img.w > 0.5) {
        // zdjęcie: łagodne dodatkowe światło z rzeźby, żeby bryła była czytelna
        let shade = mix(0.78, 1.12, ndl) * mix(0.6, 1.0, shadow);
        col = base * shade * mix(vec3(1.0), g.sun_color.rgb * 0.8 + g.sky_color.rgb * 0.35, 0.35);
    } else if (style == 0) {
        col = base * light;
    } else {
        // style analityczne: cieniowanie z wielu kierunków (czytelna rzeźba), bez kolorów pory dnia
        let nc = normalize(vec3(-grad.x * 1.6, 1.0, -grad.y * 1.6));
        let h1 = max(dot(nc, normalize(vec3(-0.6, 0.7, -0.6))), 0.0);
        let h2 = max(dot(nc, normalize(vec3(-0.9, 0.9, 0.1))), 0.0);
        let h3 = max(dot(nc, normalize(vec3(0.0, 0.8, -0.8))), 0.0);
        let hs = h1 * 0.55 + h2 * 0.25 + h3 * 0.2;
        col = base * (0.42 + 0.7 * hs) * mix(0.75, 1.0, shadow);
    }

    if (water > 0.5) {
        let wc = select(srgb(vec3(0.36, 0.60, 0.80)), srgb(vec3(0.62, 0.80, 0.93)), style == 5);
        col = select(wc * (0.4 + 0.6 * light), wc, style != 0 && style != 4);
    }

    // soczewka / wysokość względna
    let lens_mode = i32(g.lens.w + 0.5);
    if (lens_mode > 0) {
        let rd = distance(wp, g.lens.xy);
        let dh = h - g.lens.z;
        let px_m = max(fwidth(h), 0.05);
        if (lens_mode == 1 || lens_mode == 2) {
            // ta sama wysokość: pas ± tolerancji albo izolinia
            let tol = max(g.lens2.x, 0.0);
            if (lens_mode == 2 && tol > 0.0) {
                let inside = 1.0 - smoothstep(tol - px_m, tol + px_m, abs(dh));
                let lum = vec3(dot(col, vec3(0.3, 0.59, 0.11)));
                col = mix(col * 0.55 + lum * 0.1, col * vec3(1.18, 1.08, 0.9) + vec3(0.07, 0.045, 0.0), inside);
                let edge = 1.0 - smoothstep(0.0, px_m * 1.8, abs(abs(dh) - tol));
                col = mix(col, vec3(1.0, 0.78, 0.35), edge);
            }
            let line = 1.0 - smoothstep(0.0, px_m * 1.6, abs(dh));
            col = mix(col, vec3(1.0, 0.86, 0.45), line);
        } else if (lens_mode == 3) {
            let t = clamp(dh / max(g.lens2.y, 10.0), -1.0, 1.0);
            var above: vec3<f32>;
            if (t < 0.5) {
                above = mix(srgb(vec3(0.98, 0.94, 0.80)), srgb(vec3(0.97, 0.60, 0.22)), t * 2.0);
            } else {
                above = mix(srgb(vec3(0.97, 0.60, 0.22)), srgb(vec3(0.62, 0.08, 0.10)), (t - 0.5) * 2.0);
            }
            let below = mix(srgb(vec3(0.82, 0.93, 0.95)), srgb(vec3(0.04, 0.22, 0.45)), -t);
            let nc = normalize(vec3(-grad.x * 1.4, 1.0, -grad.y * 1.4));
            let shade = mix(0.55, 1.1, max(dot(nc, normalize(vec3(-0.6, 0.75, -0.5))), 0.0));
            col = select(below, above, t >= 0.0) * shade;
            // izolinia poziomu odniesienia
            let z0 = 1.0 - smoothstep(0.0, px_m * 1.8, abs(dh));
            col = mix(col, vec3(1.0), z0 * 0.95);
            // pierścienie odległości co 1 km
            let ring_w = max(fwidth(rd), 0.5) * 1.2;
            let ring = (1.0 - smoothstep(0.0, ring_w, abs(fract(rd / 1000.0 + 0.5) - 0.5) * 1000.0)) * 0.35;
            col = mix(col, vec3(1.0), ring);
        }
        // znacznik źródła soczewki
        let mk = 1.0 - smoothstep(0.0, 40.0 + in.dist * 0.004, rd);
        col += vec3(1.0, 0.9, 0.7) * mk * 0.6;
    }

    // pole widoczności
    if (g.flags.y > 0.5) {
        let lum = vec3(dot(col, vec3(0.3, 0.59, 0.11)));
        if (vr.z > 0.5) {
            let v = vs_s;
            let inside = mix(col, srgb(vec3(0.25, 0.85, 0.92)), 0.55) * 1.15 + srgb(vec3(0.02, 0.08, 0.1));
            col = mix(lum * 0.35, inside, smoothstep(0.0, 0.02, v));
        } else {
            col = lum * 0.35;
        }
    }

    // poziomice
    if (g.style.z > 0.5) {
        let interval = g.style.y;
        let fw = max(fwidth(h), 0.001);
        let d = abs(fract(h / interval + 0.5) - 0.5) * interval;
        var line = 1.0 - smoothstep(0.0, fw * 1.2, d);
        // co piąta pogrubiona
        let d5 = abs(fract(h / (interval * 5.0) + 0.5) - 0.5) * interval * 5.0;
        let major = 1.0 - smoothstep(0.0, fw * 2.0, d5);
        line = max(line * 0.55, major * 0.9);
        // znikanie gdy zbyt gęsto
        line *= 1.0 - smoothstep(interval * 0.25, interval * 0.6, fw);
        let realistic = style == 0 || style == 4;
        let cc = select(srgb(vec3(0.55, 0.33, 0.16)), vec3(0.96, 0.92, 0.82), realistic);
        col = mix(col, cc, line * select(0.75, 0.55, realistic));
    }

    // siatka kilometrowa
    if (g.style.w > 0.5) {
        let gp = (wp - g.grid_origin.xy) * g.grid_origin.z / 1000.0;
        let fw2 = fwidth(gp);
        let gl = 1.0 - smoothstep(vec2(0.0), fw2 * 1.4, abs(fract(gp + 0.5) - 0.5));
        col = mix(col, vec3(0.95, 0.9, 0.8), max(gl.x, gl.y) * 0.35);
    }

    // kursor: mały pierścień na terenie
    if (g.cursor.w > 0.5) {
        let cd = distance(wp, g.cursor.xy);
        let r = 18.0 + in.dist * 0.006;
        let ring = 1.0 - smoothstep(0.0, r * 0.18, abs(cd - r));
        col = mix(col, vec3(1.0, 0.95, 0.85), ring * 0.9);
    }

    // perspektywa powietrzna (mgła zależna od odległości i wysokości)
    let fog = 1.0 - exp(-in.dist * g.fog_color.w * (1.0 + max(0.0, 900.0 - h) / 1800.0));
    col = mix(col, g.fog_color.rgb, clamp(fog, 0.0, 0.92));
    return vec4(col, 1.0);
}
