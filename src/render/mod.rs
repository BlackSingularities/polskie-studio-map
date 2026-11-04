//! Renderer 3D (wgpu): niebo, teren kaflowy, linie szlaków i tras – do bufora poza ekranem,
//! który egui wkleja w okno mapy (wywołanie zwrotne malowania).

pub mod camera;
pub mod tiles;

use std::sync::Arc;

use bytemuck::{Pod, Zeroable};
use eframe::egui;
use eframe::egui_wgpu;

use crate::heights::Heights;
use camera::View;
use tiles::{DrawTile, TileSettings, Tiles};

const GRID: u32 = 64;
const MSAA: u32 = 4;
const COLOR: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8UnormSrgb;
const DEPTH: wgpu::TextureFormat = wgpu::TextureFormat::Depth32Float;
const TILE_STRIDE: u64 = 256;
const MAX_TILES: u64 = 8192;

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
pub struct Globals {
    pub view_proj: [[f32; 4]; 4],
    pub eye: [f32; 4],
    pub sun_dir: [f32; 4],
    pub sun_color: [f32; 4],
    pub sky_color: [f32; 4],
    pub fog_color: [f32; 4],
    pub style: [f32; 4],
    pub season: [f32; 4],
    pub lens: [f32; 4],
    pub lens2: [f32; 4],
    pub cursor: [f32; 4],
    pub shadow_rect: [f32; 4],
    pub vs_rect: [f32; 4],
    pub flags: [f32; 4],
    pub biome: [f32; 4],
    pub grid_origin: [f32; 4],
    pub land_rect: [f32; 4],
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct TileUniform {
    rect: [f32; 4],
    dem: [f32; 4],
    img: [f32; 4],
    info: [f32; 4],
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct SkyUniform {
    inv_view_proj: [[f32; 4]; 4],
    sun_dir: [f32; 4],
    zenith: [f32; 4],
    horizon: [f32; 4],
    sun_color: [f32; 4],
}

/// Odcinek linii: końce w ramce (y – wysokość rzeczywista), kolor liniowy RGBA, grubość i przesunięcie (px).
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable, Debug)]
pub struct LineInst {
    pub p0: [f32; 3],
    pub p1: [f32; 3],
    pub color: [f32; 4],
    pub wo: [f32; 2],
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct LineParams {
    viewport: [f32; 4],
}

/// Ustawienia sceny na klatkę (z interfejsu).
#[derive(Clone, Copy, Debug)]
pub struct Scene {
    pub style: u32,
    pub contours: bool,
    pub contour_interval: f32,
    pub km_grid: bool,
    pub sun_dir: [f32; 3],
    pub sun_power: f32,
    pub sun_color: [f32; 3],
    pub sky_color: [f32; 3],
    pub zenith: [f32; 3],
    pub horizon: [f32; 3],
    pub fog_density: f32,
    pub snowline: f32,
    pub snow: bool,
    pub autumn: f32,
    /// (x, z, wysokość, tryb)
    pub lens: [f32; 4],
    pub band_tol: f32,
    pub rel_range: f32,
    pub cursor: Option<[f32; 3]>,
    pub shadows: bool,
    pub viewshed: bool,
    pub biome: [f32; 3],
    pub render_scale: f32,
    pub detail_px: f32,
    pub imagery: bool,
    pub ortho: bool,
    /// Lekka mapa normalnych: drobna rzeźba z bliska.
    pub detail: bool,
    pub max_dem: usize,
    pub max_img: usize,
    /// Prostokąty map cieni i widoczności w metrach ramki: x0, z0, 1/szer., 1/wys.
    pub shadow_rect: [f32; 4],
    pub vs_rect: [f32; 4],
    /// Odległość zanikania szlaków (m).
    pub line_fade: f32,
    /// Maska pokrycia terenu (OSM) i jej prostokąt w metrach ramki.
    pub landcover: bool,
    pub land_rect: [f32; 4],
}

struct Targets {
    size: [u32; 2],
    color_ms: wgpu::TextureView,
    resolved: wgpu::TextureView,
    depth: wgpu::TextureView,
    blit_bind: wgpu::BindGroup,
}

/// Mapa rastrowa nakładana na teren (cienie, widoczność): prostokąt w metrach ramki.
pub struct OverlayTex {
    view: wgpu::TextureView,
}

pub struct Renderer {
    device: wgpu::Device,
    queue: wgpu::Queue,
    pub tiles: Tiles,
    globals_buf: wgpu::Buffer,
    globals_layout: wgpu::BindGroupLayout,
    globals_bind: wgpu::BindGroup,
    tile_buf: wgpu::Buffer,
    tile_bind: wgpu::BindGroup,
    sky_buf: wgpu::Buffer,
    sky_bind: wgpu::BindGroup,
    line_params: wgpu::Buffer,
    line_params_bind: wgpu::BindGroup,
    terrain: wgpu::RenderPipeline,
    overlay: wgpu::RenderPipeline,
    sky: wgpu::RenderPipeline,
    lines: wgpu::RenderPipeline,
    blit: wgpu::RenderPipeline,
    blit_layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    clamp_sampler: wgpu::Sampler,
    vbuf: wgpu::Buffer,
    ibuf: wgpu::Buffer,
    index_count: u32,
    targets: Option<Targets>,
    static_lines: (wgpu::Buffer, u32, u64),
    dyn_lines: (wgpu::Buffer, u32),
    shadow: OverlayTex,
    vs: OverlayTex,
    land: OverlayTex,
    water: wgpu::TextureView,
    empty_overlay: wgpu::TextureView,
    empty_land: wgpu::TextureView,
    empty_water: wgpu::TextureView,
    pub drawn: usize,
}

/// Brak wody w teksturze poziomów lustra.
pub const NO_WATER: f32 = -10_000.0;

fn overlay_view(device: &wgpu::Device, queue: &wgpu::Queue, w: u32, h: u32, data: &[u8]) -> wgpu::TextureView {
    tiles::make_texture(device, queue, wgpu::TextureFormat::R8Unorm, w, h, &[data.to_vec()], 1).create_view(&Default::default())
}

impl Renderer {
    pub fn new(rs: &egui_wgpu::RenderState, heights: Heights, ctx: egui::Context) -> Self {
        let device = rs.device.clone();
        let queue = rs.queue.clone();
        let tiles = Tiles::new(&device, &queue, heights, ctx);
        let uniform = |binding, vis, dynamic| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: vis,
            ty: wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Uniform,
                has_dynamic_offset: dynamic,
                min_binding_size: None,
            },
            count: None,
        };
        let tex = |binding| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Texture {
                sample_type: wgpu::TextureSampleType::Float { filterable: true },
                view_dimension: wgpu::TextureViewDimension::D2,
                multisampled: false,
            },
            count: None,
        };
        let vf = wgpu::ShaderStages::VERTEX_FRAGMENT;
        let globals_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("globals"),
            entries: &[
                uniform(0, vf, false),
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
                tex(2),
                tex(3),
                tex(4),
                // poziom lustra wody (R32Float, czytany też w cieniowaniu wierzchołków)
                wgpu::BindGroupLayoutEntry {
                    binding: 5,
                    visibility: wgpu::ShaderStages::VERTEX | wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: false },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
            ],
        });
        let tile_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("tile"),
            entries: &[uniform(0, vf, true)],
        });
        let sky_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("sky"),
            entries: &[uniform(0, vf, false)],
        });
        let line_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("lines"),
            entries: &[uniform(0, vf, false)],
        });
        let blit_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("blit"),
            entries: &[
                tex(0),
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });
        let buf = |label, size, usage| {
            device.create_buffer(&wgpu::BufferDescriptor {
                label: Some(label),
                size,
                usage: usage | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            })
        };
        let globals_buf = buf("globals", std::mem::size_of::<Globals>() as u64, wgpu::BufferUsages::UNIFORM);
        let tile_buf = buf("tiles", TILE_STRIDE * MAX_TILES, wgpu::BufferUsages::UNIFORM);
        let sky_buf = buf("sky", std::mem::size_of::<SkyUniform>() as u64, wgpu::BufferUsages::UNIFORM);
        let line_params = buf("line-params", 16, wgpu::BufferUsages::UNIFORM);
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("liniowy"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::FilterMode::Linear,
            anisotropy_clamp: 8,
            ..Default::default()
        });
        let clamp_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("blit"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        let empty_overlay = overlay_view(&device, &queue, 1, 1, &[255]);
        let tile_bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("tile"),
            layout: &tile_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                    buffer: &tile_buf,
                    offset: 0,
                    size: wgpu::BufferSize::new(std::mem::size_of::<TileUniform>() as u64),
                }),
            }],
        });
        let sky_bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("sky"),
            layout: &sky_layout,
            entries: &[wgpu::BindGroupEntry { binding: 0, resource: sky_buf.as_entire_binding() }],
        });
        let line_params_bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("line-params"),
            layout: &line_layout,
            entries: &[wgpu::BindGroupEntry { binding: 0, resource: line_params.as_entire_binding() }],
        });

        let module = |label, src: &str| {
            device.create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some(label),
                source: wgpu::ShaderSource::Wgsl(src.into()),
            })
        };
        let terrain_sh = module("teren", include_str!("terrain.wgsl"));
        let lines_sh = module("linie", include_str!("lines.wgsl"));
        let sky_sh = module("niebo", include_str!("sky.wgsl"));
        let blit_sh = module("blit", include_str!("blit.wgsl"));
        let layout = |label, groups: &[&wgpu::BindGroupLayout]| {
            device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some(label),
                bind_group_layouts: groups,
                push_constant_ranges: &[],
            })
        };
        let ms = wgpu::MultisampleState { count: MSAA, mask: !0, alpha_to_coverage_enabled: false };
        let terrain = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("teren"),
            layout: Some(&layout("teren", &[&globals_layout, &tile_layout, &tiles.dem_layout, &tiles.img_layout])),
            vertex: wgpu::VertexState {
                module: &terrain_sh,
                entry_point: Some("vs_main"),
                compilation_options: Default::default(),
                buffers: &[wgpu::VertexBufferLayout {
                    array_stride: 12,
                    step_mode: wgpu::VertexStepMode::Vertex,
                    attributes: &wgpu::vertex_attr_array![0 => Float32x2, 1 => Float32],
                }],
            },
            primitive: wgpu::PrimitiveState { cull_mode: None, ..Default::default() },
            depth_stencil: Some(wgpu::DepthStencilState {
                format: DEPTH,
                depth_write_enabled: true,
                depth_compare: wgpu::CompareFunction::Greater,
                stencil: Default::default(),
                bias: Default::default(),
            }),
            multisample: ms,
            fragment: Some(wgpu::FragmentState {
                module: &terrain_sh,
                entry_point: Some("fs_main"),
                compilation_options: Default::default(),
                targets: &[Some(COLOR.into())],
            }),
            multiview: None,
            cache: None,
        });
        // warstwy tematyczne: ta sama siatka co teren, półprzezroczysty obraz bez zapisu głębi
        let overlay = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("warstwy"),
            layout: Some(&layout("warstwy", &[&globals_layout, &tile_layout, &tiles.dem_layout, &tiles.img_layout])),
            vertex: wgpu::VertexState {
                module: &terrain_sh,
                entry_point: Some("vs_main"),
                compilation_options: Default::default(),
                buffers: &[wgpu::VertexBufferLayout {
                    array_stride: 12,
                    step_mode: wgpu::VertexStepMode::Vertex,
                    attributes: &wgpu::vertex_attr_array![0 => Float32x2, 1 => Float32],
                }],
            },
            primitive: wgpu::PrimitiveState { cull_mode: None, ..Default::default() },
            depth_stencil: Some(wgpu::DepthStencilState {
                format: DEPTH,
                depth_write_enabled: false,
                depth_compare: wgpu::CompareFunction::GreaterEqual,
                stencil: Default::default(),
                bias: Default::default(),
            }),
            multisample: ms,
            fragment: Some(wgpu::FragmentState {
                module: &terrain_sh,
                entry_point: Some("fs_overlay"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: COLOR,
                    blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            multiview: None,
            cache: None,
        });
        let sky = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("niebo"),
            layout: Some(&layout("niebo", &[&sky_layout])),
            vertex: wgpu::VertexState { module: &sky_sh, entry_point: Some("vs_full"), compilation_options: Default::default(), buffers: &[] },
            primitive: Default::default(),
            depth_stencil: Some(wgpu::DepthStencilState {
                format: DEPTH,
                depth_write_enabled: false,
                depth_compare: wgpu::CompareFunction::Always,
                stencil: Default::default(),
                bias: Default::default(),
            }),
            multisample: ms,
            fragment: Some(wgpu::FragmentState { module: &sky_sh, entry_point: Some("fs_sky"), compilation_options: Default::default(), targets: &[Some(COLOR.into())] }),
            multiview: None,
            cache: None,
        });
        let lines = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("linie"),
            layout: Some(&layout("linie", &[&globals_layout, &line_layout])),
            vertex: wgpu::VertexState {
                module: &lines_sh,
                entry_point: Some("vs_main"),
                compilation_options: Default::default(),
                buffers: &[wgpu::VertexBufferLayout {
                    array_stride: std::mem::size_of::<LineInst>() as u64,
                    step_mode: wgpu::VertexStepMode::Instance,
                    attributes: &wgpu::vertex_attr_array![0 => Float32x3, 1 => Float32x3, 2 => Float32x4, 3 => Float32x2],
                }],
            },
            primitive: Default::default(),
            depth_stencil: Some(wgpu::DepthStencilState {
                format: DEPTH,
                depth_write_enabled: false,
                depth_compare: wgpu::CompareFunction::GreaterEqual,
                stencil: Default::default(),
                bias: Default::default(),
            }),
            multisample: ms,
            fragment: Some(wgpu::FragmentState {
                module: &lines_sh,
                entry_point: Some("fs_main"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: COLOR,
                    blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            multiview: None,
            cache: None,
        });
        let target = rs.target_format;
        let gamma = !target.is_srgb();
        let blit = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("blit"),
            layout: Some(&layout("blit", &[&blit_layout])),
            vertex: wgpu::VertexState { module: &blit_sh, entry_point: Some("vs_blit"), compilation_options: Default::default(), buffers: &[] },
            primitive: Default::default(),
            depth_stencil: None,
            multisample: Default::default(),
            fragment: Some(wgpu::FragmentState {
                module: &blit_sh,
                entry_point: Some(if gamma { "fs_blit_gamma" } else { "fs_blit_linear" }),
                compilation_options: Default::default(),
                targets: &[Some(target.into())],
            }),
            multiview: None,
            cache: None,
        });

        // siatka kafla z fartuchem
        let mut verts: Vec<f32> = Vec::new();
        let side = GRID + 3;
        for j in 0..side {
            for i in 0..side {
                let f = |k: u32| ((k as f32 - 1.0) / GRID as f32).clamp(0.0, 1.0);
                let skirt = if i == 0 || j == 0 || i == side - 1 || j == side - 1 { 1.0 } else { 0.0 };
                verts.extend_from_slice(&[f(i), f(j), skirt]);
            }
        }
        let mut idx: Vec<u32> = Vec::new();
        for j in 0..side - 1 {
            for i in 0..side - 1 {
                let a = j * side + i;
                let (b, c, d) = (a + 1, a + side, a + side + 1);
                idx.extend_from_slice(&[a, c, b, b, c, d]);
            }
        }
        use wgpu::util::DeviceExt;
        let vbuf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor { label: Some("siatka"), contents: bytemuck::cast_slice(&verts), usage: wgpu::BufferUsages::VERTEX });
        let ibuf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor { label: Some("indeksy"), contents: bytemuck::cast_slice(&idx), usage: wgpu::BufferUsages::INDEX });
        let line_buf = |n: u64| buf("linie", n * std::mem::size_of::<LineInst>() as u64, wgpu::BufferUsages::VERTEX);
        let static_lines = (line_buf(1), 0, u64::MAX);
        let dyn_lines = (line_buf(1024), 0);
        let empty_land = tiles::make_texture(&device, &queue, wgpu::TextureFormat::Rgba8Unorm, 1, 1, &[vec![0, 0, 0, 0]], 4).create_view(&Default::default());
        let empty_water = tiles::make_texture(&device, &queue, wgpu::TextureFormat::R32Float, 1, 1, &[bytemuck::cast_slice(&[NO_WATER]).to_vec()], 4).create_view(&Default::default());
        let globals_bind = Self::make_globals_bind(&device, &globals_layout, &globals_buf, &sampler, &empty_overlay, &empty_overlay, &empty_land, &empty_water);
        Renderer {
            shadow: OverlayTex { view: empty_overlay.clone() },
            vs: OverlayTex { view: empty_overlay.clone() },
            land: OverlayTex { view: empty_land.clone() },
            empty_land,
            water: empty_water.clone(),
            empty_water,
            empty_overlay,
            device,
            queue,
            tiles,
            globals_buf,
            globals_layout,
            globals_bind,
            tile_buf,
            tile_bind,
            sky_buf,
            sky_bind,
            line_params,
            line_params_bind,
            terrain,
            overlay,
            sky,
            lines,
            blit,
            blit_layout,
            sampler,
            clamp_sampler,
            vbuf,
            ibuf,
            index_count: idx.len() as u32,
            targets: None,
            static_lines,
            dyn_lines,
            drawn: 0,
        }
    }

    fn make_globals_bind(
        device: &wgpu::Device,
        layout: &wgpu::BindGroupLayout,
        buf: &wgpu::Buffer,
        sampler: &wgpu::Sampler,
        shadow: &wgpu::TextureView,
        vs: &wgpu::TextureView,
        land: &wgpu::TextureView,
        water: &wgpu::TextureView,
    ) -> wgpu::BindGroup {
        device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("globals"),
            layout,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: buf.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::Sampler(sampler) },
                wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::TextureView(shadow) },
                wgpu::BindGroupEntry { binding: 3, resource: wgpu::BindingResource::TextureView(vs) },
                wgpu::BindGroupEntry { binding: 4, resource: wgpu::BindingResource::TextureView(land) },
                wgpu::BindGroupEntry { binding: 5, resource: wgpu::BindingResource::TextureView(water) },
            ],
        })
    }

    fn rebind_globals(&mut self) {
        self.globals_bind = Self::make_globals_bind(&self.device, &self.globals_layout, &self.globals_buf, &self.sampler, &self.shadow.view, &self.vs.view, &self.land.view, &self.water);
    }

    /// Mapa cieni (0 – cień, 255 – słońce) na prostokącie ramki; `None` usuwa.
    pub fn set_shadow(&mut self, data: Option<(&[u8], u32, u32)>) {
        self.shadow = match data {
            Some((d, w, h)) => OverlayTex { view: overlay_view(&self.device, &self.queue, w, h, d) },
            None => OverlayTex { view: self.empty_overlay.clone() },
        };
        self.rebind_globals();
    }

    /// Maska pokrycia terenu (RGBA: woda, las, skały/zarośla) i poziomy lustra wody
    /// (m n.p.m., `NO_WATER` poza wodą; ten sam prostokąt, własny bok); `None` usuwa.
    pub fn set_landcover(&mut self, data: Option<(&[u8], u32, &[f32], u32)>) {
        (self.land, self.water) = match data {
            Some((d, w, lv, lw)) => (
                OverlayTex {
                    view: tiles::make_texture(&self.device, &self.queue, wgpu::TextureFormat::Rgba8Unorm, w, w, &[d.to_vec()], 4).create_view(&Default::default()),
                },
                tiles::make_texture(&self.device, &self.queue, wgpu::TextureFormat::R32Float, lw, lw, &[bytemuck::cast_slice(lv).to_vec()], 4).create_view(&Default::default()),
            ),
            None => (OverlayTex { view: self.empty_land.clone() }, self.empty_water.clone()),
        };
        self.rebind_globals();
    }

    pub fn set_viewshed(&mut self, data: Option<(&[u8], u32, u32)>) {
        self.vs = match data {
            Some((d, w, h)) => OverlayTex { view: overlay_view(&self.device, &self.queue, w, h, d) },
            None => OverlayTex { view: self.empty_overlay.clone() },
        };
        self.rebind_globals();
    }

    fn ensure_targets(&mut self, size: [u32; 2]) {
        if self.targets.as_ref().is_some_and(|t| t.size == size) {
            return;
        }
        let mk = |format, samples, usage| {
            self.device
                .create_texture(&wgpu::TextureDescriptor {
                    label: Some("cel 3D"),
                    size: wgpu::Extent3d { width: size[0], height: size[1], depth_or_array_layers: 1 },
                    mip_level_count: 1,
                    sample_count: samples,
                    dimension: wgpu::TextureDimension::D2,
                    format,
                    usage,
                    view_formats: &[],
                })
                .create_view(&Default::default())
        };
        let color_ms = mk(COLOR, MSAA, wgpu::TextureUsages::RENDER_ATTACHMENT);
        let resolved = mk(COLOR, 1, wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING);
        let depth = mk(DEPTH, MSAA, wgpu::TextureUsages::RENDER_ATTACHMENT);
        let blit_bind = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("blit"),
            layout: &self.blit_layout,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(&resolved) },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::Sampler(&self.clamp_sampler) },
            ],
        });
        self.targets = Some(Targets { size, color_ms, resolved, depth, blit_bind });
    }

    fn upload_lines(device: &wgpu::Device, queue: &wgpu::Queue, slot: &mut wgpu::Buffer, lines: &[LineInst]) {
        let need = (lines.len().max(1) * std::mem::size_of::<LineInst>()) as u64;
        if slot.size() < need {
            *slot = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("linie"),
                size: need.next_power_of_two(),
                usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
        }
        if !lines.is_empty() {
            queue.write_buffer(slot, 0, bytemuck::cast_slice(lines));
        }
    }

    /// Linie stałe (szlaki) – wysyłane tylko po zmianie (`version`).
    pub fn set_static_lines(&mut self, version: u64, lines: &[LineInst]) {
        if self.static_lines.2 == version {
            return;
        }
        Self::upload_lines(&self.device, &self.queue, &mut self.static_lines.0, lines);
        self.static_lines.1 = lines.len() as u32;
        self.static_lines.2 = version;
    }

    pub fn static_version(&self) -> u64 {
        self.static_lines.2
    }

    /// Klatka 3D do bufora poza ekranem; zwraca wywołanie zwrotne do wklejenia w egui.
    pub fn render(&mut self, view: &View, scene: &Scene, overlays: &[(u16, f32)], dyn_lines: &[LineInst], ppp: f32) -> (Blit, Vec<DrawTile>) {
        let any = self.tiles.receive(&self.device, &self.queue);
        let _ = any;
        let size = [
            ((view.size[0] * ppp * scene.render_scale).round() as u32).max(1),
            ((view.size[1] * ppp * scene.render_scale).round() as u32).max(1),
        ];
        self.ensure_targets(size);
        let settings = TileSettings {
            target_px: scene.detail_px,
            max_dem: scene.max_dem,
            max_img: scene.max_img,
            imagery: scene.imagery,
            ortho: scene.ortho,
            overlays: overlays.to_vec(),
            // z wysoka widać daleko: bez tego przy dużym oddaleniu kafle kończą się w połowie widoku
            max_dist: (view.eye.y.abs().max(1000.0) * 60.0).clamp(80_000.0, 2_500_000.0),
        };
        let draw = self.tiles.select(view, &settings);
        self.drawn = draw.len();

        let vp = view.view_proj_rel();
        let e = view.eye;
        let lens_mode = scene.lens[3];
        let g = Globals {
            view_proj: vp.to_cols_array_2d(),
            eye: [e.x as f32, e.y as f32, e.z as f32, view.exag as f32],
            sun_dir: [scene.sun_dir[0], scene.sun_dir[1], scene.sun_dir[2], scene.sun_power],
            sun_color: [scene.sun_color[0], scene.sun_color[1], scene.sun_color[2], 0.0],
            sky_color: [scene.sky_color[0], scene.sky_color[1], scene.sky_color[2], 0.0],
            fog_color: [scene.horizon[0], scene.horizon[1], scene.horizon[2], scene.fog_density],
            style: [scene.style as f32, scene.contour_interval, scene.contours as u8 as f32, scene.km_grid as u8 as f32],
            season: [scene.snowline, scene.snow as u8 as f32, scene.autumn, 1.0],
            lens: [scene.lens[0], scene.lens[1], scene.lens[2], lens_mode],
            lens2: [scene.band_tol, scene.rel_range, 0.0, 0.0],
            cursor: match scene.cursor {
                Some(c) => [c[0], c[1], c[2], 1.0],
                None => [0.0; 4],
            },
            shadow_rect: scene.shadow_rect,
            vs_rect: scene.vs_rect,
            flags: [scene.shadows as u8 as f32, scene.viewshed as u8 as f32, scene.landcover as u8 as f32, scene.detail as u8 as f32],
            biome: [scene.biome[0], scene.biome[1], scene.biome[2], 0.0],
            grid_origin: [0.0, 0.0, 1.0, 0.0],
            land_rect: scene.land_rect,
        };
        self.queue.write_buffer(&self.globals_buf, 0, bytemuck::bytes_of(&g));
        let inv = vp.inverse();
        let sky = SkyUniform {
            inv_view_proj: inv.to_cols_array_2d(),
            sun_dir: g.sun_dir,
            zenith: [scene.zenith[0], scene.zenith[1], scene.zenith[2], 0.0],
            horizon: [scene.horizon[0], scene.horizon[1], scene.horizon[2], 0.0],
            sun_color: g.sun_color,
        };
        self.queue.write_buffer(&self.sky_buf, 0, bytemuck::bytes_of(&sky));
        let lp = LineParams { viewport: [size[0] as f32, size[1] as f32, 3.0, scene.line_fade] };
        self.queue.write_buffer(&self.line_params, 0, bytemuck::bytes_of(&lp));
        Self::upload_lines(&self.device, &self.queue, &mut self.dyn_lines.0, dyn_lines);
        self.dyn_lines.1 = dyn_lines.len() as u32;

        let n = draw.len().min(MAX_TILES as usize);
        let overlay_draws: Vec<tiles::OverlayDraw> = self
            .tiles
            .overlay_draws
            .iter()
            .filter(|o| o.tile < n)
            .take(MAX_TILES as usize - n)
            .copied()
            .collect();
        let mut tbytes = vec![0u8; (n + overlay_draws.len()) * TILE_STRIDE as usize];
        for (i, d) in draw.iter().take(n).enumerate() {
            let u = TileUniform {
                rect: d.rect,
                dem: [d.dem_xform[0], d.dem_xform[1], d.dem_xform[2], 0.0],
                img: match d.img {
                    Some((k, x)) => [x[0], x[1], x[2], if k.0 == tiles::Kind::Sat { 2.0 } else { 1.0 }],
                    None => [0.0, 0.0, 1.0, 0.0],
                },
                info: [d.tile.z as f32, 0.0, 0.0, 0.0],
            };
            let o = i * TILE_STRIDE as usize;
            tbytes[o..o + std::mem::size_of::<TileUniform>()].copy_from_slice(bytemuck::bytes_of(&u));
        }
        for (i, o) in overlay_draws.iter().enumerate() {
            let d = &draw[o.tile];
            let u = TileUniform {
                rect: d.rect,
                dem: [d.dem_xform[0], d.dem_xform[1], d.dem_xform[2], 0.0],
                img: [o.xform[0], o.xform[1], o.xform[2], 1.0],
                info: [d.tile.z as f32, o.opacity, 0.0, 0.0],
            };
            let at = (n + i) * TILE_STRIDE as usize;
            tbytes[at..at + std::mem::size_of::<TileUniform>()].copy_from_slice(bytemuck::bytes_of(&u));
        }
        if n > 0 {
            self.queue.write_buffer(&self.tile_buf, 0, &tbytes);
        }

        let t = self.targets.as_ref().unwrap();
        let mut enc = self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("3D") });
        {
            let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("3D"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &t.color_ms,
                    depth_slice: None,
                    resolve_target: Some(&t.resolved),
                    ops: wgpu::Operations { load: wgpu::LoadOp::Clear(wgpu::Color::BLACK), store: wgpu::StoreOp::Discard },
                })],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: &t.depth,
                    depth_ops: Some(wgpu::Operations { load: wgpu::LoadOp::Clear(0.0), store: wgpu::StoreOp::Discard }),
                    stencil_ops: None,
                }),
                timestamp_writes: None,
                occlusion_query_set: None,
            });
            pass.set_pipeline(&self.sky);
            pass.set_bind_group(0, &self.sky_bind, &[]);
            pass.draw(0..3, 0..1);

            pass.set_pipeline(&self.terrain);
            pass.set_bind_group(0, &self.globals_bind, &[]);
            pass.set_vertex_buffer(0, self.vbuf.slice(..));
            pass.set_index_buffer(self.ibuf.slice(..), wgpu::IndexFormat::Uint32);
            for (i, d) in draw.iter().take(n).enumerate() {
                pass.set_bind_group(1, &self.tile_bind, &[(i as u64 * TILE_STRIDE) as u32]);
                pass.set_bind_group(2, self.tiles.dem_bind(d.dem), &[]);
                pass.set_bind_group(3, self.tiles.img_bind(d.img.map(|x| x.0)), &[]);
                pass.draw_indexed(0..self.index_count, 0, 0..1);
            }

            pass.set_pipeline(&self.overlay);
            for (i, o) in overlay_draws.iter().enumerate() {
                pass.set_bind_group(1, &self.tile_bind, &[((n + i) as u64 * TILE_STRIDE) as u32]);
                pass.set_bind_group(2, self.tiles.dem_bind(draw[o.tile].dem), &[]);
                pass.set_bind_group(3, self.tiles.img_bind(Some(o.image)), &[]);
                pass.draw_indexed(0..self.index_count, 0, 0..1);
            }

            pass.set_pipeline(&self.lines);
            pass.set_bind_group(0, &self.globals_bind, &[]);
            pass.set_bind_group(1, &self.line_params_bind, &[]);
            if self.static_lines.1 > 0 {
                pass.set_vertex_buffer(0, self.static_lines.0.slice(..));
                pass.draw(0..6, 0..self.static_lines.1);
            }
            if self.dyn_lines.1 > 0 {
                pass.set_vertex_buffer(0, self.dyn_lines.0.slice(..));
                pass.draw(0..6, 0..self.dyn_lines.1);
            }
        }
        self.queue.submit(Some(enc.finish()));
        (
            Blit {
                pipeline: self.blit.clone(),
                bind: t.blit_bind.clone(),
            },
            draw,
        )
    }
}

pub struct Blit {
    pipeline: wgpu::RenderPipeline,
    bind: wgpu::BindGroup,
}

impl egui_wgpu::CallbackTrait for Blit {
    fn paint(&self, _info: egui::PaintCallbackInfo, pass: &mut wgpu::RenderPass<'static>, _res: &egui_wgpu::CallbackResources) {
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, &self.bind, &[]);
        pass.draw(0..3, 0..1);
    }
}

/// Wspólny wskaźnik do renderera (tworzony po starcie, gdy znany jest adapter wgpu).
pub type Shared = Arc<std::sync::Mutex<Renderer>>;

#[cfg(test)]
mod tests {
    fn check(src: &str) {
        let m = naga::front::wgsl::parse_str(src).unwrap_or_else(|e| panic!("{}", e.emit_to_string(src)));
        naga::valid::Validator::new(naga::valid::ValidationFlags::all(), naga::valid::Capabilities::empty())
            .validate(&m)
            .unwrap_or_else(|e| panic!("{e:?}"));
    }

    #[test]
    fn shaders_validate() {
        check(include_str!("terrain.wgsl"));
        check(include_str!("lines.wgsl"));
        check(include_str!("sky.wgsl"));
        check(include_str!("blit.wgsl"));
    }
}
