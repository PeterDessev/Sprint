//! wgpu renderer: pipelines, chunked instance buffers, offscreen MSAA
//! targets, drawing and pixel read-back. Owned by `GcodeViewer`.

use std::borrow::Cow;
use std::ops::Range;
use std::sync::mpsc;

use bytemuck::{Pod, Zeroable};
use egui_wgpu::wgpu;
use glam::Vec3;
use wgpu::util::DeviceExt;

use crate::instance::{RetractionInstance, SegmentInstance};
use crate::options::{BedSpec, RenderStyle};
use crate::scene::{self, LineVertex, MeshVertex, TubeVertex};

/// MSAA sample count of the offscreen target.
pub(crate) const MSAA_SAMPLES: u32 = 4;
/// Depth buffer format (reversed Z).
pub(crate) const DEPTH_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Depth32Float;
/// Format of the resolved texture handed to egui (egui requires Rgba8Unorm).
pub(crate) const RESOLVE_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;
/// Instances per GPU buffer; keeps every buffer far below `max_buffer_size`.
const CHUNK_INSTANCES: usize = 1 << 20;
/// Z of the bed plate (just below the first layer's bottom).
pub(crate) const BED_Z: f32 = -0.04;

/// Shader uniforms; layout matches `Globals` in `shader.wgsl`.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Pod, Zeroable)]
pub(crate) struct Globals {
    pub view_proj: [[f32; 4]; 4],
    pub eye: [f32; 4],
    pub forward: [f32; 4],
    pub key_light: [f32; 4],
    pub bed: [f32; 4],
    pub feature_colors: [[f32; 4]; 16],
    pub tool_colors: [[f32; 4]; 8],
    pub ramp: [[f32; 4]; 12],
    pub travel_color: [f32; 4],
    pub retract_color: [f32; 4],
    pub unretract_color: [f32; 4],
    pub plate_color: [f32; 4],
    pub grid_color: [f32; 4],
    pub grid_bold_color: [f32; 4],
    pub mode: [u32; 4],
    pub lines: [u32; 4],
    pub layers: [u32; 4],
    pub range: [f32; 4],
    /// Direction towards the fill light (xyz) and its intensity (w).
    pub fill_light: [f32; 4],
    /// Direction towards the rim light (xyz) and its intensity (w).
    pub rim_light: [f32; 4],
    /// Linear haze color (rgb) and depth-cue strength (a).
    pub fog: [f32; 4],
    /// Depth-cue start distance (x) and `1 / (end - start)` (y).
    pub fog_range: [f32; 4],
    /// Cavity darkening (x), projected bead thickness in pixels where
    /// shading detail starts (y) and is complete (z), exposure (w).
    pub shading: [f32; 4],
    /// Opacity of translucent ghosts (x); yzw unused.
    pub ghost: [f32; 4],
    /// Nozzle tip (xyz) and tool head mesh scale (w).
    pub head: [f32; 4],
    /// Shadow radius in mm (x), strength (y) and the height above the tip
    /// it is cast from (z); w unused.
    pub head_shadow: [f32; 4],
    /// The highlighted object as stored in the instance attributes (its
    /// index plus one; 0 = none) (x); yzw unused.
    pub objects: [u32; 4],
}

/// Bits of `Globals::mode[2]`.
pub(crate) mod flags {
    pub const PROGRESS: u32 = 1;
    pub const HIGHLIGHT: u32 = 2;
    pub const LINE_LIMIT: u32 = 4;
    pub const HOVER: u32 = 8;
    pub const LAYER_TOP: u32 = 16;
    pub const ENCODE_GAMMA: u32 = 32;
    pub const TRANSLUCENT_GHOSTS: u32 = 64;
    pub const HEAD: u32 = 128;
    pub const CURRENT_LAYER: u32 = 256;
}

/// Everything needed to draw one frame.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct DrawParams {
    pub globals: Globals,
    /// Clear color (linear, or sRGB-encoded when gamma is encoded manually).
    pub clear: [f64; 4],
    pub style: RenderStyle,
    /// Extrusion instance ranges drawn as prisms (or as lines in
    /// `RenderStyle::Lines`), in this order (front to back).
    pub prisms: Vec<Range<u32>>,
    /// Extrusion instance ranges drawn as ribbon impostors.
    pub ribbons: Vec<Range<u32>>,
    /// Ranges drawn again, after everything opaque, as translucent ghosts
    /// (prisms / lines). Empty unless ghosts are translucent.
    pub ghost_prisms: Vec<Range<u32>>,
    /// Ribbon ranges of the ghost pass.
    pub ghost_ribbons: Vec<Range<u32>>,
    pub travels: Option<Range<u32>>,
    pub retractions: bool,
    pub bed: bool,
    pub volume: bool,
    pub axes: bool,
    /// Nozzle tip when the tool head is shown (placed and scaled by
    /// `Globals::head`).
    pub tool_head: Option<Vec3>,
}

/// Offscreen color (MSAA + resolved) and depth targets of one size.
pub(crate) struct Targets {
    pub size: (u32, u32),
    msaa_view: wgpu::TextureView,
    depth_view: wgpu::TextureView,
    resolve: wgpu::Texture,
    resolve_render_view: wgpu::TextureView,
    /// Plain Rgba8Unorm view of the resolved image (what egui samples).
    pub sample_view: wgpu::TextureView,
}

/// Which target set to draw into.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Slot {
    /// The texture shown by the egui widget.
    Ui,
    /// Separate targets for `render_to_image`.
    Offscreen,
}

/// An instance array split across several vertex buffers.
struct ChunkedBuffer {
    label: &'static str,
    stride: usize,
    /// Created lazily as uploading reaches each chunk, so allocating a huge
    /// scene is spread over frames too.
    buffers: Vec<wgpu::Buffer>,
    len: usize,
    uploaded: usize,
}

impl ChunkedBuffer {
    fn new(label: &'static str, len: usize, stride: usize) -> ChunkedBuffer {
        ChunkedBuffer {
            label,
            stride,
            buffers: Vec::with_capacity(len.div_ceil(CHUNK_INSTANCES)),
            len,
            uploaded: 0,
        }
    }

    /// Upload up to `budget` more instances from `data`; returns how many
    /// were written.
    fn upload<T: Pod>(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        data: &[T],
        budget: usize,
    ) -> usize {
        let mut written = 0;
        while written < budget && self.uploaded < self.len.min(data.len()) {
            let chunk = self.uploaded / CHUNK_INSTANCES;
            let in_chunk = self.uploaded % CHUNK_INSTANCES;
            let chunk_end = ((chunk + 1) * CHUNK_INSTANCES).min(self.len);
            let n = (chunk_end - self.uploaded).min(budget - written);
            if self.buffers.len() <= chunk {
                let count = (self.len - chunk * CHUNK_INSTANCES).min(CHUNK_INSTANCES);
                self.buffers
                    .push(device.create_buffer(&wgpu::BufferDescriptor {
                        label: Some(self.label),
                        size: (count * self.stride) as u64,
                        usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                        mapped_at_creation: false,
                    }));
            }
            let Some(buffer) = self.buffers.get(chunk) else {
                break;
            };
            let Some(src) = data.get(self.uploaded..self.uploaded + n) else {
                break;
            };
            let offset = (in_chunk * std::mem::size_of::<T>()) as u64;
            queue.write_buffer(buffer, offset, bytemuck::cast_slice(src));
            self.uploaded += n;
            written += n;
        }
        written
    }

    fn is_complete(&self) -> bool {
        self.uploaded >= self.len
    }

    /// Split an instance range into per-buffer draws, limited to what has
    /// been uploaded.
    fn draws(&self, range: Range<u32>) -> impl Iterator<Item = (&wgpu::Buffer, Range<u32>)> + '_ {
        let end = (range.end as usize).min(self.uploaded);
        let start = (range.start as usize).min(end);
        self.buffers.iter().enumerate().filter_map(move |(i, b)| {
            let lo = i * CHUNK_INSTANCES;
            let hi = lo + CHUNK_INSTANCES;
            let s = start.max(lo);
            let e = end.min(hi);
            (s < e).then(|| (b, (s - lo) as u32..(e - lo) as u32))
        })
    }
}

/// Instance data waiting to be uploaded in slices.
struct Pending {
    extrusions: Vec<SegmentInstance>,
    travels: Vec<SegmentInstance>,
    retractions: Vec<RetractionInstance>,
}

struct GpuScene {
    extrusions: ChunkedBuffer,
    travels: ChunkedBuffer,
    retractions: ChunkedBuffer,
    pending: Option<Pending>,
}

struct MeshBuffers {
    vertices: wgpu::Buffer,
    indices: wgpu::Buffer,
    count: u32,
}

impl MeshBuffers {
    fn new<V: Pod>(device: &wgpu::Device, label: &str, mesh: &scene::Mesh<V>) -> MeshBuffers {
        MeshBuffers {
            vertices: device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some(label),
                contents: bytemuck::cast_slice(&mesh.vertices),
                usage: wgpu::BufferUsages::VERTEX,
            }),
            indices: device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some(label),
                contents: bytemuck::cast_slice(&mesh.indices),
                usage: wgpu::BufferUsages::INDEX,
            }),
            count: mesh.indices.len() as u32,
        }
    }
}

struct Pipelines {
    tube: wgpu::RenderPipeline,
    ribbon: wgpu::RenderPipeline,
    extrusion_lines: wgpu::RenderPipeline,
    tube_ghost: wgpu::RenderPipeline,
    ribbon_ghost: wgpu::RenderPipeline,
    extrusion_lines_ghost: wgpu::RenderPipeline,
    travel: wgpu::RenderPipeline,
    marker: wgpu::RenderPipeline,
    mesh: wgpu::RenderPipeline,
    head: wgpu::RenderPipeline,
    head_xray: wgpu::RenderPipeline,
    bed: wgpu::RenderPipeline,
    overlay: wgpu::RenderPipeline,
}

/// The GPU side of the viewer.
pub(crate) struct Renderer {
    device: wgpu::Device,
    queue: wgpu::Queue,
    /// Render through an sRGB view (hardware encoding); false on backends
    /// without view-format reinterpretation, where the shader encodes.
    srgb_views: bool,
    globals: wgpu::Buffer,
    bind_group: wgpu::BindGroup,
    pipelines: Pipelines,
    tube: MeshBuffers,
    marker: MeshBuffers,
    bed_vertices: wgpu::Buffer,
    bed_indices: wgpu::Buffer,
    volume: wgpu::Buffer,
    volume_count: u32,
    axes: MeshBuffers,
    /// Tool head around the origin; the shader places and scales it.
    head: MeshBuffers,
    scene: Option<GpuScene>,
    ui_targets: Option<Targets>,
    offscreen_targets: Option<Targets>,
    readback: Option<(wgpu::Buffer, u64)>,
}

/// The WGSL source of all viewer shaders.
pub(crate) const SHADER: &str = include_str!("shader.wgsl");

/// Shader source, with per-sample shading of lit surfaces when requested.
pub(crate) fn shader_source(sample_shading: bool) -> Cow<'static, str> {
    if sample_shading {
        Cow::Owned(SHADER.replace("/*SAMPLE*/", "@interpolate(perspective, sample)"))
    } else {
        Cow::Borrowed(SHADER)
    }
}

/// Sample-rate shading may be available on every native backend except GL
/// (Vulkan devices without `sampleRateShading` reject it).
fn sample_shading_supported(device: &wgpu::Device) -> bool {
    matches!(
        device.adapter_info().backend,
        wgpu::Backend::Vulkan | wgpu::Backend::Dx12 | wgpu::Backend::Metal
    )
}

/// Compile the shaders and build every pipeline, with per-sample shading
/// where the backend may support it. A device that rejects the per-sample
/// shader (validation error, which wgpu's default handler turns into a
/// panic) gets the per-pixel variant instead.
fn build_pipelines(
    device: &wgpu::Device,
    layout: &wgpu::PipelineLayout,
    color_format: wgpu::TextureFormat,
) -> Pipelines {
    let module = |sample_shading: bool| {
        device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("sprint-viewer shader"),
            source: wgpu::ShaderSource::Wgsl(shader_source(sample_shading)),
        })
    };
    if sample_shading_supported(device) {
        let scope = device.push_error_scope(wgpu::ErrorFilter::Validation);
        let pipelines = create_pipelines(device, &module(true), layout, color_format);
        match pollster::block_on(scope.pop()) {
            None => return pipelines,
            Some(err) => log::warn!(
                "sprint-viewer: per-sample shading is not supported ({err}); \
                 shading per pixel instead"
            ),
        }
    }
    create_pipelines(device, &module(false), layout, color_format)
}

/// Bytes per row of a read-back copy of a `width` pixels wide RGBA8 image.
fn padded_row_bytes(width: u32) -> u64 {
    let align = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT as u64;
    (width as u64 * 4).div_ceil(align) * align
}

fn segment_attributes() -> [wgpu::VertexAttribute; 8] {
    wgpu::vertex_attr_array![
        2 => Float32x3, 3 => Float32, 4 => Float32x3, 5 => Float32,
        6 => Uint32, 7 => Uint32, 8 => Uint32, 9 => Uint32
    ]
}

impl Renderer {
    /// Create pipelines and static geometry for `device`.
    pub fn new(device: &wgpu::Device, queue: &wgpu::Queue) -> Renderer {
        let srgb_views = device.adapter_info().backend != wgpu::Backend::Gl;
        let color_format = if srgb_views {
            RESOLVE_FORMAT.add_srgb_suffix()
        } else {
            RESOLVE_FORMAT
        };
        let globals = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("sprint-viewer globals"),
            size: std::mem::size_of::<Globals>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("sprint-viewer globals layout"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX | wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: wgpu::BufferSize::new(std::mem::size_of::<Globals>() as u64),
                },
                count: None,
            }],
        });
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("sprint-viewer globals"),
            layout: &layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: globals.as_entire_binding(),
            }],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("sprint-viewer pipeline layout"),
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });
        let pipelines = build_pipelines(device, &pipeline_layout, color_format);

        let tube = MeshBuffers::new(
            device,
            "sprint-viewer tube template",
            &scene::tube_template(),
        );
        let marker = MeshBuffers::new(device, "sprint-viewer marker", &scene::marker_template());
        let bed_vertices = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("sprint-viewer bed"),
            size: 4 * 12,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let bed_indices = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("sprint-viewer bed indices"),
            contents: bytemuck::cast_slice(&[0u32, 1, 2, 0, 2, 3]),
            usage: wgpu::BufferUsages::INDEX,
        });
        let volume = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("sprint-viewer build volume"),
            size: (24 * std::mem::size_of::<LineVertex>()) as u64,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let axes = MeshBuffers::new(
            device,
            "sprint-viewer axes",
            &scene::axes_mesh(Vec3::ZERO, 20.0),
        );
        let head = MeshBuffers::new(
            device,
            "sprint-viewer tool head",
            &scene::tool_head_mesh(Vec3::ZERO),
        );
        let mut renderer = Renderer {
            device: device.clone(),
            queue: queue.clone(),
            srgb_views,
            globals,
            bind_group,
            pipelines,
            tube,
            marker,
            bed_vertices,
            bed_indices,
            volume,
            volume_count: 24,
            axes,
            head,
            scene: None,
            ui_targets: None,
            offscreen_targets: None,
            readback: None,
        };
        renderer.set_bed(&BedSpec::default());
        renderer
    }

    /// True when the shader must encode sRGB itself.
    pub fn encodes_gamma(&self) -> bool {
        !self.srgb_views
    }

    /// The wgpu device.
    pub fn device(&self) -> &wgpu::Device {
        &self.device
    }

    /// Largest supported target dimension.
    pub fn max_texture_size(&self) -> u32 {
        self.device.limits().max_texture_dimension_2d
    }

    /// Rebuild bed-dependent geometry.
    pub fn set_bed(&mut self, bed: &BedSpec) {
        let quad = scene::bed_quad(bed, BED_Z);
        self.queue
            .write_buffer(&self.bed_vertices, 0, bytemuck::cast_slice(&quad));
        let lines = scene::build_volume_lines(bed);
        self.volume_count = lines.len() as u32;
        self.queue
            .write_buffer(&self.volume, 0, bytemuck::cast_slice(&lines));
        let s = bed.sanitized();
        let length = (s.size[0].min(s.size[1]) * 0.1).clamp(8.0, 40.0);
        self.axes = MeshBuffers::new(
            &self.device,
            "sprint-viewer axes",
            &scene::axes_mesh(Vec3::ZERO, length),
        );
    }

    /// Replace the scene; data is uploaded by [`Renderer::upload_step`].
    pub fn set_scene(
        &mut self,
        extrusions: Vec<SegmentInstance>,
        travels: Vec<SegmentInstance>,
        retractions: Vec<RetractionInstance>,
    ) {
        self.scene = Some(GpuScene {
            extrusions: ChunkedBuffer::new(
                "sprint-viewer extrusions",
                extrusions.len(),
                SegmentInstance::SIZE,
            ),
            travels: ChunkedBuffer::new(
                "sprint-viewer travels",
                travels.len(),
                SegmentInstance::SIZE,
            ),
            retractions: ChunkedBuffer::new(
                "sprint-viewer retractions",
                retractions.len(),
                RetractionInstance::SIZE,
            ),
            pending: Some(Pending {
                extrusions,
                travels,
                retractions,
            }),
        });
    }

    /// Drop all scene buffers.
    pub fn clear_scene(&mut self) {
        self.scene = None;
    }

    /// Upload up to `budget_bytes` of pending instance data. Returns true
    /// once everything is on the GPU (or there is no scene).
    pub fn upload_step(&mut self, budget_bytes: usize) -> bool {
        let Some(scene) = self.scene.as_mut() else {
            return true;
        };
        let Some(pending) = scene.pending.as_ref() else {
            return true;
        };
        let mut budget = budget_bytes.max(SegmentInstance::SIZE);
        let n = scene.extrusions.upload(
            &self.device,
            &self.queue,
            &pending.extrusions,
            budget / SegmentInstance::SIZE,
        );
        budget = budget.saturating_sub(n * SegmentInstance::SIZE);
        if scene.extrusions.is_complete() && budget > 0 {
            let n = scene.travels.upload(
                &self.device,
                &self.queue,
                &pending.travels,
                budget / SegmentInstance::SIZE,
            );
            budget = budget.saturating_sub(n * SegmentInstance::SIZE);
        }
        if scene.travels.is_complete() && budget > 0 {
            scene.retractions.upload(
                &self.device,
                &self.queue,
                &pending.retractions,
                budget / RetractionInstance::SIZE,
            );
        }
        let done = scene.extrusions.is_complete()
            && scene.travels.is_complete()
            && scene.retractions.is_complete();
        if done {
            scene.pending = None;
        }
        done
    }

    /// Instances uploaded so far: (extrusions, travels, retractions).
    pub fn uploaded(&self) -> (usize, usize, usize) {
        self.scene.as_ref().map_or((0, 0, 0), |s| {
            (
                s.extrusions.uploaded,
                s.travels.uploaded,
                s.retractions.uploaded,
            )
        })
    }

    /// True while instance data is still being uploaded.
    pub fn is_uploading(&self) -> bool {
        self.scene.as_ref().is_some_and(|s| s.pending.is_some())
    }

    /// Make sure the targets of `slot` have the given size. Returns true if
    /// they were (re)created. When the GPU cannot allocate them, the slot is
    /// left without targets (drawing into it does nothing).
    pub fn ensure_targets(&mut self, slot: Slot, width: u32, height: u32) -> bool {
        let max = self.max_texture_size().max(1);
        let size = (width.clamp(1, max), height.clamp(1, max));
        let current = match slot {
            Slot::Ui => &self.ui_targets,
            Slot::Offscreen => &self.offscreen_targets,
        };
        if current.as_ref().is_some_and(|t| t.size == size) {
            return false;
        }
        // Free the old targets first so a resize does not need both at once.
        match slot {
            Slot::Ui => self.ui_targets = None,
            Slot::Offscreen => self.offscreen_targets = None,
        }
        // Huge sizes can exhaust video memory: report that instead of
        // letting wgpu's uncaptured-error handler panic.
        let oom = self.device.push_error_scope(wgpu::ErrorFilter::OutOfMemory);
        let targets = self.create_targets(size);
        if let Some(err) = pollster::block_on(oom.pop()) {
            log::warn!(
                "sprint-viewer: cannot allocate {}x{} render targets: {err}",
                size.0,
                size.1
            );
            return true;
        }
        match slot {
            Slot::Ui => self.ui_targets = Some(targets),
            Slot::Offscreen => self.offscreen_targets = Some(targets),
        }
        true
    }

    /// The size `render_to_image` can actually produce for a request of
    /// `width x height`: within the texture limit, and small enough that the
    /// read-back buffer fits `max_buffer_size` (scaled down keeping the
    /// aspect ratio).
    pub fn readback_size(&self, width: u32, height: u32) -> (u32, u32) {
        let max = self.max_texture_size().max(1);
        let (w, h) = (width.clamp(1, max), height.clamp(1, max));
        let max_bytes = self.device.limits().max_buffer_size.max(1);
        let bytes = padded_row_bytes(w) * h as u64;
        if bytes <= max_bytes {
            return (w, h);
        }
        let scale = (max_bytes as f64 / bytes as f64).sqrt();
        let w = ((w as f64 * scale) as u32).clamp(1, w);
        let rows = (max_bytes / padded_row_bytes(w)).clamp(1, u32::MAX as u64) as u32;
        let h = ((h as f64 * scale) as u32).clamp(1, rows);
        (w, h)
    }

    /// The targets of a slot, if created.
    pub fn targets(&self, slot: Slot) -> Option<&Targets> {
        match slot {
            Slot::Ui => self.ui_targets.as_ref(),
            Slot::Offscreen => self.offscreen_targets.as_ref(),
        }
    }

    fn create_targets(&self, size: (u32, u32)) -> Targets {
        let extent = wgpu::Extent3d {
            width: size.0,
            height: size.1,
            depth_or_array_layers: 1,
        };
        let render_format = if self.srgb_views {
            RESOLVE_FORMAT.add_srgb_suffix()
        } else {
            RESOLVE_FORMAT
        };
        let msaa = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("sprint-viewer msaa color"),
            size: extent,
            mip_level_count: 1,
            sample_count: MSAA_SAMPLES,
            dimension: wgpu::TextureDimension::D2,
            format: render_format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            view_formats: &[],
        });
        let depth = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("sprint-viewer depth"),
            size: extent,
            mip_level_count: 1,
            sample_count: MSAA_SAMPLES,
            dimension: wgpu::TextureDimension::D2,
            format: DEPTH_FORMAT,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            view_formats: &[],
        });
        let view_formats: &[wgpu::TextureFormat] = if self.srgb_views {
            &[wgpu::TextureFormat::Rgba8UnormSrgb]
        } else {
            &[]
        };
        let resolve = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("sprint-viewer resolved color"),
            size: extent,
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: RESOLVE_FORMAT,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                | wgpu::TextureUsages::TEXTURE_BINDING
                | wgpu::TextureUsages::COPY_SRC,
            view_formats,
        });
        let resolve_render_view = resolve.create_view(&wgpu::TextureViewDescriptor {
            label: Some("sprint-viewer resolve target"),
            format: Some(render_format),
            ..Default::default()
        });
        let sample_view = resolve.create_view(&wgpu::TextureViewDescriptor {
            label: Some("sprint-viewer resolved image"),
            format: Some(RESOLVE_FORMAT),
            ..Default::default()
        });
        Targets {
            size,
            msaa_view: msaa.create_view(&wgpu::TextureViewDescriptor::default()),
            depth_view: depth.create_view(&wgpu::TextureViewDescriptor::default()),
            resolve,
            resolve_render_view,
            sample_view,
        }
    }

    /// Draw one frame into the targets of `slot` and submit it.
    pub fn render(&mut self, slot: Slot, params: &DrawParams) {
        self.queue
            .write_buffer(&self.globals, 0, bytemuck::bytes_of(&params.globals));
        let Some(targets) = self.targets(slot) else {
            return;
        };
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("sprint-viewer frame"),
            });
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("sprint-viewer scene"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &targets.msaa_view,
                    depth_slice: None,
                    resolve_target: Some(&targets.resolve_render_view),
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color {
                            r: params.clear[0],
                            g: params.clear[1],
                            b: params.clear[2],
                            a: params.clear[3],
                        }),
                        store: wgpu::StoreOp::Discard,
                    },
                })],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: &targets.depth_view,
                    depth_ops: Some(wgpu::Operations {
                        load: wgpu::LoadOp::Clear(0.0),
                        store: wgpu::StoreOp::Discard,
                    }),
                    stencil_ops: None,
                }),
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_bind_group(0, &self.bind_group, &[]);
            self.draw_scene(&mut pass, params);
        }
        self.queue.submit(std::iter::once(encoder.finish()));
    }

    /// Draw extrusion ranges as prisms and ribbons (or all as lines) with
    /// the given pipelines.
    fn draw_extrusions(
        &self,
        pass: &mut wgpu::RenderPass<'_>,
        style: RenderStyle,
        (prisms, ribbons): (&[Range<u32>], &[Range<u32>]),
        (tube, ribbon, lines): (
            &wgpu::RenderPipeline,
            &wgpu::RenderPipeline,
            &wgpu::RenderPipeline,
        ),
    ) {
        let Some(scene) = &self.scene else {
            return;
        };
        match style {
            RenderStyle::Tubes => {
                if !prisms.is_empty() {
                    pass.set_pipeline(tube);
                    pass.set_vertex_buffer(0, self.tube.vertices.slice(..));
                    pass.set_index_buffer(self.tube.indices.slice(..), wgpu::IndexFormat::Uint32);
                    for batch in prisms {
                        for (buffer, range) in scene.extrusions.draws(batch.clone()) {
                            pass.set_vertex_buffer(1, buffer.slice(..));
                            pass.draw_indexed(0..self.tube.count, 0, range);
                        }
                    }
                }
                if !ribbons.is_empty() {
                    pass.set_pipeline(ribbon);
                    for batch in ribbons {
                        for (buffer, range) in scene.extrusions.draws(batch.clone()) {
                            pass.set_vertex_buffer(0, buffer.slice(..));
                            pass.draw(0..4, range);
                        }
                    }
                }
            }
            RenderStyle::Lines => {
                if prisms.is_empty() && ribbons.is_empty() {
                    return;
                }
                pass.set_pipeline(lines);
                for batch in prisms.iter().chain(ribbons) {
                    for (buffer, range) in scene.extrusions.draws(batch.clone()) {
                        pass.set_vertex_buffer(0, buffer.slice(..));
                        pass.draw(0..2, range);
                    }
                }
            }
        }
    }

    fn draw_scene(&self, pass: &mut wgpu::RenderPass<'_>, params: &DrawParams) {
        let p = &self.pipelines;
        self.draw_extrusions(
            pass,
            params.style,
            (&params.prisms, &params.ribbons),
            (&p.tube, &p.ribbon, &p.extrusion_lines),
        );
        if let Some(scene) = &self.scene
            && params.retractions
            && scene.retractions.uploaded > 0
        {
            pass.set_pipeline(&p.marker);
            pass.set_vertex_buffer(0, self.marker.vertices.slice(..));
            pass.set_index_buffer(self.marker.indices.slice(..), wgpu::IndexFormat::Uint32);
            for (buffer, range) in scene.retractions.draws(0..u32::MAX) {
                pass.set_vertex_buffer(1, buffer.slice(..));
                pass.draw_indexed(0..self.marker.count, 0, range);
            }
        }
        if params.tool_head.is_some() {
            pass.set_vertex_buffer(0, self.head.vertices.slice(..));
            pass.set_index_buffer(self.head.indices.slice(..), wgpu::IndexFormat::Uint32);
            // First a faint silhouette where the print hides the nozzle
            // (inverted depth test, before the nozzle's own depth is
            // written, so it never shows through itself), then the nozzle.
            pass.set_pipeline(&p.head_xray);
            pass.draw_indexed(0..self.head.count, 0, 0..1);
            pass.set_pipeline(&p.head);
            pass.draw_indexed(0..self.head.count, 0, 0..1);
        }
        if params.axes {
            pass.set_pipeline(&p.mesh);
            pass.set_vertex_buffer(0, self.axes.vertices.slice(..));
            pass.set_index_buffer(self.axes.indices.slice(..), wgpu::IndexFormat::Uint32);
            pass.draw_indexed(0..self.axes.count, 0, 0..1);
        }
        if params.bed {
            pass.set_pipeline(&p.bed);
            pass.set_vertex_buffer(0, self.bed_vertices.slice(..));
            pass.set_index_buffer(self.bed_indices.slice(..), wgpu::IndexFormat::Uint32);
            pass.draw_indexed(0..6, 0, 0..1);
        }
        if params.volume {
            pass.set_pipeline(&p.overlay);
            pass.set_vertex_buffer(0, self.volume.slice(..));
            pass.draw(0..self.volume_count, 0..1);
        }
        // Translucent ghosts after everything opaque (the bed included):
        // depth tested against the printed part, not hiding each other.
        self.draw_extrusions(
            pass,
            params.style,
            (&params.ghost_prisms, &params.ghost_ribbons),
            (&p.tube_ghost, &p.ribbon_ghost, &p.extrusion_lines_ghost),
        );
        if let (Some(scene), Some(range)) = (&self.scene, params.travels.clone()) {
            pass.set_pipeline(&p.travel);
            for (buffer, r) in scene.travels.draws(range) {
                pass.set_vertex_buffer(0, buffer.slice(..));
                pass.draw(0..2, r);
            }
        }
    }

    /// Copy the resolved image of `slot` to the CPU as tightly packed RGBA8
    /// (sRGB-encoded) rows. Blocks until the GPU is done.
    pub fn read_pixels(&mut self, slot: Slot) -> Option<(u32, u32, Vec<u8>)> {
        let (width, height) = self.targets(slot)?.size;
        let unpadded = width as u64 * 4;
        let padded = padded_row_bytes(width);
        let size = padded * height as u64;
        if size > self.device.limits().max_buffer_size {
            log::warn!("sprint-viewer: {width}x{height} is too big to read back");
            return None;
        }
        if self.readback.as_ref().is_none_or(|(_, s)| *s < size) {
            let buffer = self.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("sprint-viewer readback"),
                size,
                usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
                mapped_at_creation: false,
            });
            self.readback = Some((buffer, size));
        }
        let (buffer, _) = self.readback.as_ref()?;
        let targets = self.targets(slot)?;
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("sprint-viewer readback"),
            });
        encoder.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture: &targets.resolve,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(padded as u32),
                    rows_per_image: Some(height),
                },
            },
            wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
        );
        self.queue.submit(std::iter::once(encoder.finish()));
        let slice = buffer.slice(..size);
        let (tx, rx) = mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |result| {
            let _ = tx.send(result);
        });
        if let Err(err) = self.device.poll(wgpu::PollType::wait_indefinitely()) {
            log::warn!("sprint-viewer: GPU poll failed during read-back: {err}");
        }
        let mapped = matches!(rx.recv(), Ok(Ok(())));
        if !mapped {
            log::warn!("sprint-viewer: mapping the read-back buffer failed");
            buffer.unmap();
            return None;
        }
        let pixels = {
            let view = slice.get_mapped_range().ok()?;
            let mut out = Vec::with_capacity((unpadded * height as u64) as usize);
            for row in view.chunks(padded as usize).take(height as usize) {
                out.extend_from_slice(row.get(..unpadded as usize)?);
            }
            out
        };
        buffer.unmap();
        Some((width, height, pixels))
    }

    /// Fraction of the scene's instance data already uploaded (1 when idle).
    pub fn upload_progress(&self) -> f32 {
        let Some(s) = &self.scene else {
            return 1.0;
        };
        let total = s.extrusions.len + s.travels.len + s.retractions.len;
        if total == 0 {
            return 1.0;
        }
        let done = s.extrusions.uploaded + s.travels.uploaded + s.retractions.uploaded;
        (done as f32 / total as f32).clamp(0.0, 1.0)
    }
}

fn create_pipelines(
    device: &wgpu::Device,
    module: &wgpu::ShaderModule,
    layout: &wgpu::PipelineLayout,
    color_format: wgpu::TextureFormat,
) -> Pipelines {
    let seg_attrs = segment_attributes();
    let segment_instances = wgpu::VertexBufferLayout {
        array_stride: SegmentInstance::SIZE as u64,
        step_mode: wgpu::VertexStepMode::Instance,
        attributes: &seg_attrs,
    };
    let tube_attrs = wgpu::vertex_attr_array![0 => Float32x4, 1 => Float32x4];
    let tube_vertices = wgpu::VertexBufferLayout {
        array_stride: std::mem::size_of::<TubeVertex>() as u64,
        step_mode: wgpu::VertexStepMode::Vertex,
        attributes: &tube_attrs,
    };
    let marker_attrs = wgpu::vertex_attr_array![0 => Float32x3, 1 => Float32x3];
    let marker_vertices = wgpu::VertexBufferLayout {
        array_stride: std::mem::size_of::<MeshVertex>() as u64,
        step_mode: wgpu::VertexStepMode::Vertex,
        attributes: &marker_attrs,
    };
    let retraction_attrs =
        wgpu::vertex_attr_array![2 => Float32x3, 3 => Uint32, 4 => Uint32, 5 => Uint32];
    let retraction_instances = wgpu::VertexBufferLayout {
        array_stride: RetractionInstance::SIZE as u64,
        step_mode: wgpu::VertexStepMode::Instance,
        attributes: &retraction_attrs,
    };
    let mesh_attrs = wgpu::vertex_attr_array![0 => Float32x3, 1 => Float32x3, 2 => Unorm8x4];
    let mesh_vertices = wgpu::VertexBufferLayout {
        array_stride: std::mem::size_of::<MeshVertex>() as u64,
        step_mode: wgpu::VertexStepMode::Vertex,
        attributes: &mesh_attrs,
    };
    let head_vertices = mesh_vertices.clone();
    let head_vertices_xray = mesh_vertices.clone();
    let line_attrs = wgpu::vertex_attr_array![0 => Float32x3, 1 => Unorm8x4];
    let line_vertices = wgpu::VertexBufferLayout {
        array_stride: std::mem::size_of::<LineVertex>() as u64,
        step_mode: wgpu::VertexStepMode::Vertex,
        attributes: &line_attrs,
    };
    let bed_attrs = wgpu::vertex_attr_array![0 => Float32x3];
    let bed_vertices = wgpu::VertexBufferLayout {
        array_stride: 12,
        step_mode: wgpu::VertexStepMode::Vertex,
        attributes: &bed_attrs,
    };

    let make = |label: &str,
                vs: &str,
                fs: &str,
                buffers: &[Option<wgpu::VertexBufferLayout<'_>>],
                topology: wgpu::PrimitiveTopology,
                cull: Option<wgpu::Face>,
                depth_write: bool,
                blend: bool,
                compare: wgpu::CompareFunction| {
        device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some(label),
            layout: Some(layout),
            vertex: wgpu::VertexState {
                module,
                entry_point: Some(vs),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                buffers,
            },
            primitive: wgpu::PrimitiveState {
                topology,
                strip_index_format: None,
                front_face: wgpu::FrontFace::Ccw,
                cull_mode: cull,
                unclipped_depth: false,
                polygon_mode: wgpu::PolygonMode::Fill,
                conservative: false,
            },
            depth_stencil: Some(wgpu::DepthStencilState {
                format: DEPTH_FORMAT,
                depth_write_enabled: Some(depth_write),
                depth_compare: Some(compare),
                stencil: wgpu::StencilState::default(),
                bias: wgpu::DepthBiasState::default(),
            }),
            multisample: wgpu::MultisampleState {
                count: MSAA_SAMPLES,
                mask: !0,
                alpha_to_coverage_enabled: false,
            },
            fragment: Some(wgpu::FragmentState {
                module,
                entry_point: Some(fs),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: color_format,
                    blend: blend.then_some(wgpu::BlendState::ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            multiview_mask: None,
            cache: None,
        })
    };
    use wgpu::CompareFunction::{GreaterEqual, Less};
    use wgpu::PrimitiveTopology::{LineList, TriangleList, TriangleStrip};
    Pipelines {
        tube: make(
            "sprint-viewer tubes",
            "vs_tube",
            "fs_tube",
            &[Some(tube_vertices.clone()), Some(segment_instances.clone())],
            TriangleList,
            Some(wgpu::Face::Back),
            true,
            false,
            GreaterEqual,
        ),
        ribbon: make(
            "sprint-viewer ribbons",
            "vs_ribbon",
            "fs_ribbon",
            &[Some(segment_instances.clone())],
            TriangleStrip,
            None,
            true,
            false,
            GreaterEqual,
        ),
        extrusion_lines: make(
            "sprint-viewer extrusion lines",
            "vs_extrusion_line",
            "fs_flat",
            &[Some(segment_instances.clone())],
            LineList,
            None,
            true,
            false,
            GreaterEqual,
        ),
        tube_ghost: make(
            "sprint-viewer tubes (ghosts)",
            "vs_tube_ghost",
            "fs_flat",
            &[Some(tube_vertices.clone()), Some(segment_instances.clone())],
            TriangleList,
            Some(wgpu::Face::Back),
            false,
            true,
            GreaterEqual,
        ),
        ribbon_ghost: make(
            "sprint-viewer ribbons (ghosts)",
            "vs_ribbon_ghost",
            "fs_flat",
            &[Some(segment_instances.clone())],
            TriangleStrip,
            None,
            false,
            true,
            GreaterEqual,
        ),
        extrusion_lines_ghost: make(
            "sprint-viewer extrusion lines (ghosts)",
            "vs_extrusion_line_ghost",
            "fs_flat",
            &[Some(segment_instances.clone())],
            LineList,
            None,
            false,
            true,
            GreaterEqual,
        ),
        travel: make(
            "sprint-viewer travel",
            "vs_travel",
            "fs_flat",
            &[Some(segment_instances)],
            LineList,
            None,
            false,
            true,
            GreaterEqual,
        ),
        marker: make(
            "sprint-viewer markers",
            "vs_marker",
            "fs_lit",
            &[Some(marker_vertices), Some(retraction_instances)],
            TriangleList,
            Some(wgpu::Face::Back),
            true,
            false,
            GreaterEqual,
        ),
        mesh: make(
            "sprint-viewer meshes",
            "vs_mesh",
            "fs_lit",
            &[Some(mesh_vertices)],
            TriangleList,
            Some(wgpu::Face::Back),
            true,
            false,
            GreaterEqual,
        ),
        head: make(
            "sprint-viewer tool head",
            "vs_head",
            "fs_head",
            &[Some(head_vertices)],
            TriangleList,
            Some(wgpu::Face::Back),
            true,
            false,
            GreaterEqual,
        ),
        head_xray: make(
            "sprint-viewer tool head (occluded)",
            "vs_head",
            "fs_head_xray",
            &[Some(head_vertices_xray)],
            TriangleList,
            Some(wgpu::Face::Back),
            false,
            true,
            Less,
        ),
        bed: make(
            "sprint-viewer bed",
            "vs_bed",
            "fs_bed",
            &[Some(bed_vertices)],
            TriangleList,
            None,
            false,
            true,
            GreaterEqual,
        ),
        overlay: make(
            "sprint-viewer overlay lines",
            "vs_overlay",
            "fs_flat",
            &[Some(line_vertices)],
            LineList,
            None,
            false,
            true,
            GreaterEqual,
        ),
    }
}

/// Create a device and queue without a window (tests, thumbnails,
/// examples). Prefers a discrete GPU and falls back to a software adapter
/// (WARP / llvmpipe) when no hardware adapter is available. The
/// `WGPU_BACKEND` environment variable (e.g. `dx12`, `vulkan`, `gl`)
/// restricts the backends tried. Returns `None` when no adapter at all can
/// be created.
pub fn headless_device() -> Option<(wgpu::Device, wgpu::Queue)> {
    let backends = wgpu::Backends::from_env().unwrap_or(wgpu::Backends::PRIMARY);
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
        backends,
        ..wgpu::InstanceDescriptor::new_without_display_handle()
    });
    let adapter = [false, true].into_iter().find_map(|fallback| {
        pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            force_fallback_adapter: fallback,
            compatible_surface: None,
            apply_limit_buckets: false,
        }))
        .ok()
    })?;
    let info = adapter.get_info();
    log::info!(
        "sprint-viewer: headless adapter {} ({:?})",
        info.name,
        info.backend
    );
    let base = if info.backend == wgpu::Backend::Gl {
        wgpu::Limits::downlevel_webgl2_defaults()
    } else {
        wgpu::Limits::default()
    };
    pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        label: Some("sprint-viewer headless device"),
        required_limits: base.using_resolution(adapter.limits()),
        ..Default::default()
    }))
    .ok()
}
