use std::{borrow::Cow, sync::Arc};

use super::super::markers::{MARKER_VERTICES, MarkerInstance, MarkerVertex};
use super::super::mesh::{
    AssetMesh, MeshInstance, MeshPrimitive, MeshUniforms, MeshVertex, PrimitiveKind, primitive,
    view_projection_uniform,
};
use super::super::overlays::{OverlayConfig, TextOverlay};
use super::super::shaders::{
    MARKER_SHADER_SOURCE, MESH_SHADER_SOURCE, QUAD_INDICES, QUAD_VERTICES, QuadVertex,
    SHADER_SOURCE,
};
use super::layout;
use super::overlay_updates;
use super::panels::ViewerOverlays;
use super::selection;
use super::{MannyMesh, MeshPreviewResources, MeshResources, PrimitiveBuffers, ViewerState};
use anyhow::{Context, Result};
use bytemuck::cast_slice;
use glam::{Mat4, Vec3};
use wgpu::util::DeviceExt;
use winit::{dpi::PhysicalSize, window::Window};

use crate::cli::{LayoutPreset, MinimapPreset, PanelPreset};
use crate::scene::{CameraProjector, MovementScrubber, ViewerScene};
use crate::texture::{
    PreviewTexture, generate_placeholder_texture, prepare_rgba_upload, preview_color,
};
use crate::ui_layout::{MinimapConstraints, PanelSize, UiLayout};

/// Bundles the wgpu objects tied to the viewer window.
struct WgpuBootstrap {
    surface: wgpu::Surface<'static>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    surface_format: wgpu::TextureFormat,
    present_mode: wgpu::PresentMode,
    alpha_mode: wgpu::CompositeAlphaMode,
}

/// Holds the decoded preview texture (or fallback) and its clear color.
struct PreviewBundle {
    preview: PreviewTexture,
    background: wgpu::Color,
}

/// GPU resources for the main texture and its bind group.
struct TextureResources {
    texture: wgpu::Texture,
    view: wgpu::TextureView,
    sampler: wgpu::Sampler,
    bind_group_layout: wgpu::BindGroupLayout,
    bind_group: wgpu::BindGroup,
}

/// Render pipelines and buffers needed for the plate and markers.
struct RenderResources {
    quad_pipeline: wgpu::RenderPipeline,
    minimap_pipeline: wgpu::RenderPipeline,
    quad_vertex_buffer: wgpu::Buffer,
    quad_index_buffer: wgpu::Buffer,
    quad_index_count: u32,
    minimap_marker_vertex_buffer: wgpu::Buffer,
    minimap_marker_instance_buffer: wgpu::Buffer,
    minimap_marker_capacity: usize,
}

const MESH_DEPTH_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Depth32Float;
const MESH_PREVIEW_PANEL_SIZE: PanelSize = PanelSize {
    width: 220.0,
    height: 220.0,
};

/// Overlay textures plus layout hints for the HUD panels.
struct OverlaySetup {
    overlays: ViewerOverlays,
    audio_panel: Option<PanelSize>,
    timeline_panel: Option<PanelSize>,
    scrubber_panel: Option<PanelSize>,
    minimap_constraints: MinimapConstraints,
}

/// Enumerates the on-screen overlays rendered into HUD panels.
#[derive(Clone, Copy)]
enum OverlayKind {
    Audio,
    Scrubber,
    Timeline,
}

impl OverlayKind {
    fn base_config(self) -> OverlayConfig {
        match self {
            OverlayKind::Audio => OverlayConfig {
                width: 520,
                height: 144,
                padding_x: 8,
                padding_y: 8,
                label: "audio-overlay",
            },
            OverlayKind::Scrubber => OverlayConfig {
                width: 520,
                height: 176,
                padding_x: 8,
                padding_y: 8,
                label: "scrubber-overlay",
            },
            OverlayKind::Timeline => OverlayConfig {
                width: 640,
                height: 224,
                padding_x: 8,
                padding_y: 8,
                label: "timeline-overlay",
            },
        }
    }
}

/// Bootstraps wgpu, uploads the decoded bitmap, prepares overlays, and
/// computes an initial camera projection/minimap layout before handing back a
/// ready-to-render `ViewerState`. This is where we establish render pipelines,
/// bind groups, and marker buffers so frame rendering stays lightweight.
pub(super) async fn new(
    window: Arc<Window>,
    asset_name: &str,
    asset_bytes: Vec<u8>,
    decode_result: Result<PreviewTexture>,
    scene: Option<Arc<ViewerScene>>,
    enable_audio_overlay: bool,
    layout_preset: Option<LayoutPreset>,
    manny_mesh: Option<AssetMesh>,
) -> Result<ViewerState> {
    let size = window.inner_size();
    let layout_preset = layout_preset.unwrap_or_default();

    let wgpu = bootstrap_wgpu(window.clone()).await?;
    let preview_bundle = resolve_preview(asset_name, &asset_bytes, decode_result);

    let texture_width = preview_bundle.preview.width;
    let texture_height = preview_bundle.preview.height;
    let texture_size = PhysicalSize::new(texture_width.max(1), texture_height.max(1));
    let texture_aspect = (texture_width.max(1) as f32) / (texture_height.max(1) as f32);

    println!(
        "Preview texture sized {}x{} ({} bytes of source)",
        texture_width,
        texture_height,
        asset_bytes.len()
    );

    let scene_ref = scene.as_deref();
    let camera_projector = scene_ref.and_then(|scene| scene.camera_projector(texture_aspect));
    // The camera projector, when present, feeds the perspective math for scene markers.
    // Rendering falls back to a top-down projection in render.rs when this is None.
    log_scene_camera(scene_ref);

    let texture_resources =
        create_texture_resources(&wgpu.device, &wgpu.queue, &preview_bundle.preview)?;

    let scrubber = scene_ref.and_then(MovementScrubber::new);
    let scrubber_available = scrubber.is_some();
    let timeline_available = scene_ref
        .and_then(|scene| scene.timeline.as_ref())
        .is_some();

    let overlay_setup = build_overlays(
        &wgpu.device,
        &wgpu.queue,
        &texture_resources.bind_group_layout,
        size,
        &layout_preset,
        enable_audio_overlay,
        scrubber_available,
        timeline_available,
    )?;

    let render_resources = create_render_resources(
        &wgpu.device,
        &texture_resources.bind_group_layout,
        wgpu.surface_format,
    );
    let mesh_resources = create_mesh_resources(&wgpu.device, size, wgpu.surface_format, manny_mesh);

    let selected_entity = initial_selected_entity(scene_ref);

    let OverlaySetup {
        overlays,
        audio_panel,
        timeline_panel,
        scrubber_panel,
        minimap_constraints,
    } = overlay_setup;

    let mesh_preview_panel = if mesh_resources.manny.is_some() {
        Some(MESH_PREVIEW_PANEL_SIZE)
    } else {
        None
    };

    let ui_layout = UiLayout::new(
        size,
        audio_panel,
        timeline_panel,
        scrubber_panel,
        minimap_constraints,
        mesh_preview_panel,
    )?;

    let mut state = assemble_viewer_state(
        window,
        size,
        wgpu,
        preview_bundle,
        texture_size,
        scene,
        camera_projector,
        selected_entity,
        scrubber,
        ui_layout,
        overlays,
        texture_resources,
        render_resources,
        Some(mesh_resources),
    );

    state.surface.configure(&state.device, &state.config);
    selection::print_selected_entity(&state);
    overlay_updates::refresh_scene_overlays(&mut state);
    layout::apply_panel_layouts(&mut state);

    Ok(state)
}

async fn bootstrap_wgpu(window: Arc<Window>) -> Result<WgpuBootstrap> {
    let instance = wgpu::Instance::default();
    let surface = instance
        .create_surface(window.clone())
        .context("creating wgpu surface")?;

    let adapter = instance
        .request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            force_fallback_adapter: false,
            compatible_surface: Some(&surface),
        })
        .await
        .context("requesting wgpu adapter")?;

    let (device, queue) = adapter
        .request_device(
            &wgpu::DeviceDescriptor {
                label: Some("grim-viewer-device"),
                required_features: wgpu::Features::empty(),
                required_limits: wgpu::Limits::default(),
            },
            None,
        )
        .await
        .context("requesting wgpu device")?;

    let surface_caps = surface.get_capabilities(&adapter);
    let surface_format = surface_caps
        .formats
        .iter()
        .copied()
        .find(|format| format.is_srgb())
        .unwrap_or(surface_caps.formats[0]);
    let present_mode = surface_caps
        .present_modes
        .iter()
        .copied()
        .find(|mode| *mode == wgpu::PresentMode::Mailbox)
        .or(Some(wgpu::PresentMode::Fifo))
        .unwrap_or(wgpu::PresentMode::Fifo);
    let alpha_mode = surface_caps
        .alpha_modes
        .first()
        .copied()
        .unwrap_or(wgpu::CompositeAlphaMode::Opaque);

    Ok(WgpuBootstrap {
        surface,
        device,
        queue,
        surface_format,
        present_mode,
        alpha_mode,
    })
}

fn resolve_preview(
    asset_name: &str,
    asset_bytes: &[u8],
    decode_result: Result<PreviewTexture>,
) -> PreviewBundle {
    let (preview, background) = match decode_result {
        Ok(texture) => {
            println!(
                "Decoded BM frame: {}x{} ({} frames, codec {}, format {})",
                texture.width, texture.height, texture.frame_count, texture.codec, texture.format
            );
            if let Some(stats) = texture.depth_stats {
                println!(
                    "  depth range (raw 16-bit): 0x{min:04X} – 0x{max:04X}",
                    min = stats.min,
                    max = stats.max
                );
                println!(
                    "  depth pixels zero {zero} / {total}",
                    zero = stats.zero_pixels,
                    total = stats.total_pixels()
                );
                if texture.depth_preview {
                    println!("  preview mapped to normalized depth values");
                } else {
                    println!("  preview uses paired base bitmap for RGB");
                }
            }
            (texture, wgpu::Color::BLACK)
        }
        Err(err) => {
            eprintln!("[grim_viewer] falling back to placeholder texture: {err:?}");
            // Substitute a deterministic placeholder so the viewer stays interactive after decode failures.
            let placeholder = generate_placeholder_texture(asset_bytes, asset_name);
            let color = preview_color(asset_bytes);
            (placeholder, color)
        }
    };
    PreviewBundle {
        preview,
        background,
    }
}

fn log_scene_camera(scene: Option<&ViewerScene>) {
    let Some(scene) = scene else {
        return;
    };

    if let Some(setup) = scene.active_setup() {
        println!("[grim_viewer] active camera setup: {}", setup);
    }
    if let Some(camera) = scene.camera.as_ref() {
        println!(
            "  camera eye ({:.3}, {:.3}, {:.3}) interest ({:.3}, {:.3}, {:.3}) fov {:.2} roll {:.2}",
            camera.position[0],
            camera.position[1],
            camera.position[2],
            camera.interest[0],
            camera.interest[1],
            camera.interest[2],
            camera.fov_degrees,
            camera.roll_degrees
        );
    }
}

fn create_texture_resources(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    preview: &PreviewTexture,
) -> Result<TextureResources> {
    let texture_extent = wgpu::Extent3d {
        width: preview.width,
        height: preview.height,
        depth_or_array_layers: 1,
    };

    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("grim-viewer-texture"),
        size: texture_extent,
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8UnormSrgb,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
    let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
        label: Some("grim-viewer-sampler"),
        address_mode_u: wgpu::AddressMode::ClampToEdge,
        address_mode_v: wgpu::AddressMode::ClampToEdge,
        address_mode_w: wgpu::AddressMode::ClampToEdge,
        mag_filter: wgpu::FilterMode::Nearest,
        min_filter: wgpu::FilterMode::Nearest,
        mipmap_filter: wgpu::FilterMode::Nearest,
        ..Default::default()
    });

    let upload = prepare_rgba_upload(preview.width, preview.height, &preview.data)?;
    queue.write_texture(
        wgpu::ImageCopyTexture {
            texture: &texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        upload.pixels(),
        wgpu::ImageDataLayout {
            offset: 0,
            bytes_per_row: Some(upload.bytes_per_row()),
            rows_per_image: Some(preview.height),
        },
        texture_extent,
    );

    let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("asset-bind-group-layout"),
        entries: &[
            wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Texture {
                    multisampled: false,
                    sample_type: wgpu::TextureSampleType::Float { filterable: true },
                    view_dimension: wgpu::TextureViewDimension::D2,
                },
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 1,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                count: None,
            },
        ],
    });

    let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("asset-bind-group"),
        layout: &bind_group_layout,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(&view),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: wgpu::BindingResource::Sampler(&sampler),
            },
        ],
    });

    Ok(TextureResources {
        texture,
        view,
        sampler,
        bind_group_layout,
        bind_group,
    })
}

fn build_overlays(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    bind_group_layout: &wgpu::BindGroupLayout,
    window_size: PhysicalSize<u32>,
    layout_preset: &LayoutPreset,
    enable_audio_overlay: bool,
    scrubber_available: bool,
    timeline_available: bool,
) -> Result<OverlaySetup> {
    let audio_preset = layout_preset.audio.as_ref();
    let scrubber_preset = layout_preset.scrubber.as_ref();
    let timeline_preset = layout_preset.timeline.as_ref();

    let audio_enabled =
        enable_audio_overlay && audio_preset.map(PanelPreset::enabled).unwrap_or(true);
    let (audio_overlay, audio_panel) = build_overlay(
        device,
        queue,
        bind_group_layout,
        window_size,
        OverlayKind::Audio,
        audio_preset,
        audio_enabled,
    )?;

    let scrubber_enabled =
        scrubber_available && scrubber_preset.map(PanelPreset::enabled).unwrap_or(true);
    let (scrubber_overlay, scrubber_panel) = build_overlay(
        device,
        queue,
        bind_group_layout,
        window_size,
        OverlayKind::Scrubber,
        scrubber_preset,
        scrubber_enabled,
    )?;

    let timeline_enabled =
        timeline_available && timeline_preset.map(PanelPreset::enabled).unwrap_or(true);
    let (timeline_overlay, timeline_panel) = build_overlay(
        device,
        queue,
        bind_group_layout,
        window_size,
        OverlayKind::Timeline,
        timeline_preset,
        timeline_enabled,
    )?;

    let overlays = ViewerOverlays::new(audio_overlay, timeline_overlay, scrubber_overlay);
    let minimap_constraints = minimap_constraints_from_preset(layout_preset.minimap.as_ref());

    Ok(OverlaySetup {
        overlays,
        audio_panel,
        timeline_panel,
        scrubber_panel,
        minimap_constraints,
    })
}

fn minimap_constraints_from_preset(preset: Option<&MinimapPreset>) -> MinimapConstraints {
    let mut constraints = MinimapConstraints::default();
    if let Some(preset) = preset {
        if let Some(min_side) = preset.min_side {
            constraints.min_side = min_side;
        }
        if let Some(preferred_fraction) = preset.preferred_fraction {
            constraints.preferred_fraction = preferred_fraction;
        }
        if let Some(max_fraction) = preset.max_fraction {
            constraints.max_fraction = max_fraction;
        }
    }
    constraints
}

fn build_overlay(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    bind_group_layout: &wgpu::BindGroupLayout,
    window_size: PhysicalSize<u32>,
    kind: OverlayKind,
    preset: Option<&PanelPreset>,
    enabled: bool,
) -> Result<(Option<TextOverlay>, Option<PanelSize>)> {
    if !enabled {
        return Ok((None, None));
    }

    let config = kind.base_config().with_preset(preset);
    let panel = PanelSize::from(&config);
    let overlay = TextOverlay::new(device, queue, bind_group_layout, window_size, config)?;
    Ok((Some(overlay), Some(panel)))
}

fn assemble_viewer_state(
    window: Arc<Window>,
    size: PhysicalSize<u32>,
    wgpu: WgpuBootstrap,
    preview_bundle: PreviewBundle,
    texture_size: PhysicalSize<u32>,
    scene: Option<Arc<ViewerScene>>,
    camera_projector: Option<CameraProjector>,
    selected_entity: Option<usize>,
    scrubber: Option<MovementScrubber>,
    ui_layout: UiLayout,
    overlays: ViewerOverlays,
    texture_resources: TextureResources,
    render_resources: RenderResources,
    mesh: Option<MeshResources>,
) -> ViewerState {
    let TextureResources {
        texture,
        view,
        sampler,
        bind_group_layout: _,
        bind_group,
    } = texture_resources;

    let RenderResources {
        quad_pipeline,
        minimap_pipeline,
        quad_vertex_buffer,
        quad_index_buffer,
        quad_index_count,
        minimap_marker_vertex_buffer,
        minimap_marker_instance_buffer,
        minimap_marker_capacity,
    } = render_resources;

    ViewerState {
        window,
        surface: wgpu.surface,
        device: wgpu.device,
        queue: wgpu.queue,
        config: wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format: wgpu.surface_format,
            width: size.width.max(1),
            height: size.height.max(1),
            present_mode: wgpu.present_mode,
            alpha_mode: wgpu.alpha_mode,
            view_formats: vec![],
            desired_maximum_frame_latency: 1,
        },
        size,
        pipeline: quad_pipeline,
        quad_vertex_buffer,
        quad_index_buffer,
        quad_index_count,
        bind_group,
        _texture: texture,
        _texture_view: view,
        _sampler: sampler,
        overlays,
        background: preview_bundle.background,
        texture_size,
        scene,
        selected_entity,
        scrubber,
        camera_projector,
        minimap_pipeline,
        minimap_marker_vertex_buffer,
        minimap_marker_instance_buffer,
        minimap_marker_capacity,
        mesh,
        ui_layout,
        mesh_preview_angle: 0.0,
    }
}

fn create_mesh_resources(
    device: &wgpu::Device,
    size: PhysicalSize<u32>,
    surface_format: wgpu::TextureFormat,
    manny_mesh: Option<AssetMesh>,
) -> MeshResources {
    let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("mesh-uniform-layout"),
        entries: &[wgpu::BindGroupLayoutEntry {
            binding: 0,
            visibility: wgpu::ShaderStages::VERTEX,
            ty: wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Uniform,
                has_dynamic_offset: false,
                min_binding_size: wgpu::BufferSize::new(std::mem::size_of::<MeshUniforms>() as u64),
            },
            count: None,
        }],
    });

    let initial_uniform = view_projection_uniform(Mat4::IDENTITY);
    let uniform_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("mesh-uniform-buffer"),
        contents: cast_slice(&[initial_uniform]),
        usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
    });

    let preview_uniform_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("mesh-preview-uniform-buffer"),
        contents: cast_slice(&[initial_uniform]),
        usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
    });

    let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("mesh-uniform-bind-group"),
        layout: &bind_group_layout,
        entries: &[wgpu::BindGroupEntry {
            binding: 0,
            resource: uniform_buffer.as_entire_binding(),
        }],
    });

    let preview_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("mesh-preview-uniform-bind-group"),
        layout: &bind_group_layout,
        entries: &[wgpu::BindGroupEntry {
            binding: 0,
            resource: preview_uniform_buffer.as_entire_binding(),
        }],
    });

    let mesh_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("mesh-shader"),
        source: wgpu::ShaderSource::Wgsl(Cow::Borrowed(MESH_SHADER_SOURCE)),
    });

    let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("mesh-pipeline-layout"),
        bind_group_layouts: &[&bind_group_layout],
        push_constant_ranges: &[],
    });

    let vertex_layout = wgpu::VertexBufferLayout {
        array_stride: std::mem::size_of::<MeshVertex>() as u64,
        step_mode: wgpu::VertexStepMode::Vertex,
        attributes: &wgpu::vertex_attr_array![0 => Float32x3, 1 => Float32x3],
    };

    let instance_layout = wgpu::VertexBufferLayout {
        array_stride: std::mem::size_of::<MeshInstance>() as u64,
        step_mode: wgpu::VertexStepMode::Instance,
        attributes: &[
            wgpu::VertexAttribute {
                offset: 0,
                shader_location: 2,
                format: wgpu::VertexFormat::Float32x4,
            },
            wgpu::VertexAttribute {
                offset: 16,
                shader_location: 3,
                format: wgpu::VertexFormat::Float32x4,
            },
            wgpu::VertexAttribute {
                offset: 32,
                shader_location: 4,
                format: wgpu::VertexFormat::Float32x4,
            },
            wgpu::VertexAttribute {
                offset: 48,
                shader_location: 5,
                format: wgpu::VertexFormat::Float32x4,
            },
            wgpu::VertexAttribute {
                offset: 64,
                shader_location: 6,
                format: wgpu::VertexFormat::Float32x4,
            },
        ],
    };

    let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("mesh-pipeline"),
        layout: Some(&pipeline_layout),
        vertex: wgpu::VertexState {
            module: &mesh_shader,
            entry_point: "mesh_vs_main",
            buffers: &[vertex_layout, instance_layout],
            compilation_options: wgpu::PipelineCompilationOptions::default(),
        },
        fragment: Some(wgpu::FragmentState {
            module: &mesh_shader,
            entry_point: "mesh_fs_main",
            targets: &[Some(wgpu::ColorTargetState {
                format: surface_format,
                blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                write_mask: wgpu::ColorWrites::ALL,
            })],
            compilation_options: wgpu::PipelineCompilationOptions::default(),
        }),
        primitive: wgpu::PrimitiveState {
            cull_mode: Some(wgpu::Face::Back),
            ..wgpu::PrimitiveState::default()
        },
        depth_stencil: Some(wgpu::DepthStencilState {
            format: MESH_DEPTH_FORMAT,
            depth_write_enabled: true,
            depth_compare: wgpu::CompareFunction::LessEqual,
            stencil: wgpu::StencilState::default(),
            bias: wgpu::DepthBiasState::default(),
        }),
        multisample: wgpu::MultisampleState::default(),
        multiview: None,
    });

    let sphere_buffers = upload_primitive(device, "mesh-sphere", primitive(PrimitiveKind::Sphere));
    let cube_buffers = upload_primitive(device, "mesh-cube", primitive(PrimitiveKind::Cube));
    let cone_buffers = upload_primitive(device, "mesh-cone", primitive(PrimitiveKind::Cone));

    let initial_capacity = 8usize;
    let instance_buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("mesh-instance-buffer"),
        size: (initial_capacity * std::mem::size_of::<MeshInstance>()) as u64,
        usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });

    let preview_instance_buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("mesh-preview-instance-buffer"),
        size: std::mem::size_of::<MeshInstance>() as u64,
        usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });

    let (depth_texture, depth_view) = create_mesh_depth_texture(device, size);

    let manny_resources = manny_mesh.map(|asset| {
        let AssetMesh {
            name,
            primitive,
            triangle_count,
            bounds_min,
            bounds_max,
            radius,
            insert_offset,
        } = asset;
        let label = name.as_deref().unwrap_or("mesh-manny");
        let vertex_count = primitive.vertices.len();
        println!(
            "[grim_viewer] Manny mesh loaded: {} vertices, {} triangles, bounds min {:?}, max {:?}, radius {:?}, insert {:?}",
            vertex_count, triangle_count, bounds_min, bounds_max, radius, insert_offset
        );
        let buffers = upload_primitive(device, label, primitive);
        let bounds_center = [
            (bounds_min[0] + bounds_max[0]) * 0.5,
            (bounds_min[1] + bounds_max[1]) * 0.5,
            (bounds_min[2] + bounds_max[2]) * 0.5,
        ];
        let center_after_offset = if let Some(offset) = insert_offset {
            [
                bounds_center[0] - offset[0],
                bounds_center[1] - offset[1],
                bounds_center[2] - offset[2],
            ]
        } else {
            bounds_center
        };
        let half_extents = [
            (bounds_max[0] - bounds_min[0]) * 0.5,
            (bounds_max[1] - bounds_min[1]) * 0.5,
            (bounds_max[2] - bounds_min[2]) * 0.5,
        ];
        let center_matrix = Mat4::from_translation(-Vec3::from_array(center_after_offset));
        let insert_matrix = insert_offset
            .map(|offset| Mat4::from_translation(-Vec3::from_array(offset)))
            .unwrap_or(Mat4::IDENTITY);
        let preview_center_matrix = center_matrix * insert_matrix;
        let max_half_extent = half_extents
            .iter()
            .copied()
            .fold(0.0_f32, |acc, value| acc.max(value.abs()));
        MannyMesh {
            buffers,
            radius,
            insert_offset,
            preview_center_matrix,
            max_half_extent,
        }
    });

    MeshResources {
        pipeline,
        bind_group,
        uniform_buffer,
        depth_texture,
        depth_view,
        instance_buffer,
        instance_capacity: initial_capacity,
        sphere: sphere_buffers,
        cube: cube_buffers,
        cone: cone_buffers,
        manny: manny_resources,
        preview: MeshPreviewResources {
            bind_group: preview_bind_group,
            uniform_buffer: preview_uniform_buffer,
            instance_buffer: preview_instance_buffer,
        },
    }
}

fn upload_primitive(
    device: &wgpu::Device,
    label: &str,
    primitive: MeshPrimitive,
) -> PrimitiveBuffers {
    let vertex_label = format!("{label}-vertex-buffer");
    let vertex_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some(&vertex_label),
        contents: cast_slice(&primitive.vertices),
        usage: wgpu::BufferUsages::VERTEX,
    });

    let index_label = format!("{label}-index-buffer");
    let index_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some(&index_label),
        contents: cast_slice(&primitive.indices),
        usage: wgpu::BufferUsages::INDEX,
    });

    PrimitiveBuffers {
        vertex: vertex_buffer,
        index: index_buffer,
        index_count: primitive.indices.len() as u32,
    }
}

fn create_render_resources(
    device: &wgpu::Device,
    bind_group_layout: &wgpu::BindGroupLayout,
    surface_format: wgpu::TextureFormat,
) -> RenderResources {
    let quad_vertex_layout = wgpu::VertexBufferLayout {
        array_stride: std::mem::size_of::<QuadVertex>() as u64,
        step_mode: wgpu::VertexStepMode::Vertex,
        attributes: &wgpu::vertex_attr_array![0 => Float32x2, 1 => Float32x2],
    };

    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("asset-shader"),
        source: wgpu::ShaderSource::Wgsl(Cow::Borrowed(SHADER_SOURCE)),
    });

    let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("asset-pipeline-layout"),
        bind_group_layouts: &[bind_group_layout],
        push_constant_ranges: &[],
    });

    let quad_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("asset-pipeline"),
        layout: Some(&pipeline_layout),
        vertex: wgpu::VertexState {
            module: &shader,
            entry_point: "vs_main",
            buffers: &[quad_vertex_layout],
            compilation_options: wgpu::PipelineCompilationOptions::default(),
        },
        fragment: Some(wgpu::FragmentState {
            module: &shader,
            entry_point: "fs_main",
            targets: &[Some(wgpu::ColorTargetState {
                format: surface_format,
                blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                write_mask: wgpu::ColorWrites::ALL,
            })],
            compilation_options: wgpu::PipelineCompilationOptions::default(),
        }),
        primitive: wgpu::PrimitiveState::default(),
        depth_stencil: None,
        multisample: wgpu::MultisampleState::default(),
        multiview: None,
    });

    let quad_vertex_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("asset-quad-vertex-buffer"),
        contents: cast_slice(&QUAD_VERTICES),
        usage: wgpu::BufferUsages::VERTEX,
    });
    let quad_index_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("asset-quad-index-buffer"),
        contents: cast_slice(&QUAD_INDICES),
        usage: wgpu::BufferUsages::INDEX,
    });
    let quad_index_count = QUAD_INDICES.len() as u32;

    let marker_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("marker-shader"),
        source: wgpu::ShaderSource::Wgsl(Cow::Borrowed(MARKER_SHADER_SOURCE)),
    });

    let marker_pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("marker-pipeline-layout"),
        bind_group_layouts: &[],
        push_constant_ranges: &[],
    });

    // The minimap pipeline reuses the shared marker shader with its own label so render
    // passes can bind it independently from the plate pipeline.
    let minimap_pipeline = create_marker_pipeline(
        device,
        &marker_pipeline_layout,
        &marker_shader,
        surface_format,
        "minimap-marker-pipeline",
    );

    let minimap_marker_vertex_buffer =
        device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("minimap-marker-vertex-buffer"),
            contents: cast_slice(&MARKER_VERTICES),
            usage: wgpu::BufferUsages::VERTEX,
        });

    let initial_marker_capacity = 4usize;
    let minimap_marker_instance_buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("minimap-marker-instance-buffer"),
        size: (initial_marker_capacity * std::mem::size_of::<MarkerInstance>()) as u64,
        usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });

    RenderResources {
        quad_pipeline,
        minimap_pipeline,
        quad_vertex_buffer,
        quad_index_buffer,
        quad_index_count,
        minimap_marker_vertex_buffer,
        minimap_marker_instance_buffer,
        minimap_marker_capacity: initial_marker_capacity,
    }
}

fn create_marker_pipeline(
    device: &wgpu::Device,
    layout: &wgpu::PipelineLayout,
    shader: &wgpu::ShaderModule,
    surface_format: wgpu::TextureFormat,
    label: &'static str,
) -> wgpu::RenderPipeline {
    let marker_vertex_layout = wgpu::VertexBufferLayout {
        array_stride: std::mem::size_of::<MarkerVertex>() as u64,
        step_mode: wgpu::VertexStepMode::Vertex,
        attributes: &wgpu::vertex_attr_array![0 => Float32x2],
    };

    let marker_instance_layout = wgpu::VertexBufferLayout {
        array_stride: std::mem::size_of::<MarkerInstance>() as u64,
        step_mode: wgpu::VertexStepMode::Instance,
        attributes: &[
            wgpu::VertexAttribute {
                offset: 0,
                shader_location: 1,
                format: wgpu::VertexFormat::Float32x2,
            },
            wgpu::VertexAttribute {
                offset: 8,
                shader_location: 2,
                format: wgpu::VertexFormat::Float32,
            },
            wgpu::VertexAttribute {
                offset: 12,
                shader_location: 3,
                format: wgpu::VertexFormat::Float32,
            },
            wgpu::VertexAttribute {
                offset: 16,
                shader_location: 4,
                format: wgpu::VertexFormat::Float32,
            },
            wgpu::VertexAttribute {
                offset: 20,
                shader_location: 5,
                format: wgpu::VertexFormat::Float32x3,
            },
            wgpu::VertexAttribute {
                offset: 32,
                shader_location: 6,
                format: wgpu::VertexFormat::Float32,
            },
        ],
    };

    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some(label),
        layout: Some(layout),
        vertex: wgpu::VertexState {
            module: shader,
            entry_point: "vs_main",
            buffers: &[marker_vertex_layout, marker_instance_layout],
            compilation_options: wgpu::PipelineCompilationOptions::default(),
        },
        fragment: Some(wgpu::FragmentState {
            module: shader,
            entry_point: "fs_main",
            targets: &[Some(wgpu::ColorTargetState {
                format: surface_format,
                blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                write_mask: wgpu::ColorWrites::ALL,
            })],
            compilation_options: wgpu::PipelineCompilationOptions::default(),
        }),
        primitive: wgpu::PrimitiveState::default(),
        depth_stencil: None,
        multisample: wgpu::MultisampleState::default(),
        multiview: None,
    })
}

pub(super) fn create_mesh_depth_texture(
    device: &wgpu::Device,
    size: PhysicalSize<u32>,
) -> (wgpu::Texture, wgpu::TextureView) {
    let extent = wgpu::Extent3d {
        width: size.width.max(1),
        height: size.height.max(1),
        depth_or_array_layers: 1,
    };
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("mesh-depth-texture"),
        size: extent,
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: MESH_DEPTH_FORMAT,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
        view_formats: &[],
    });
    let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
    (texture, view)
}

fn initial_selected_entity(scene: Option<&ViewerScene>) -> Option<usize> {
    let scene = scene?;
    if scene.entities.is_empty() {
        return None;
    }
    scene
        .entities
        .iter()
        .enumerate()
        .find(|(_, entity)| entity.position.is_some())
        .map(|(idx, _)| idx)
        .or(Some(0))
}
