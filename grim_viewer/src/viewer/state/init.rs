use std::{borrow::Cow, sync::Arc};

use super::super::markers::{MARKER_VERTICES, MarkerInstance, MarkerVertex};
use super::super::overlays::{OverlayConfig, TextOverlay};
use super::super::shaders::{
    MARKER_SHADER_SOURCE, QUAD_INDICES, QUAD_VERTICES, QuadVertex, SHADER_SOURCE,
};
use super::ViewerState;
use super::layout;
use super::overlay_updates;
use super::selection;
use anyhow::{Context, Result};
use bytemuck::cast_slice;
use wgpu::util::DeviceExt;
use winit::window::Window;

use crate::cli::{LayoutPreset, PanelPreset};
use crate::scene::{MovementScrubber, ViewerScene};
use crate::texture::{
    PreviewTexture, generate_placeholder_texture, prepare_rgba_upload, preview_color,
};
use crate::ui_layout::{MinimapConstraints, PanelSize, UiLayout};

pub(super) async fn new(
    window: Arc<Window>,
    asset_name: &str,
    asset_bytes: Vec<u8>,
    decode_result: Result<PreviewTexture>,
    scene: Option<Arc<ViewerScene>>,
    enable_audio_overlay: bool,
    layout_preset: Option<LayoutPreset>,
) -> Result<ViewerState> {
    let size = window.inner_size();

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
            let placeholder = generate_placeholder_texture(&asset_bytes, asset_name);
            let color = preview_color(&asset_bytes);
            (placeholder, color)
        }
    };
    let texture_width = preview.width;
    let texture_height = preview.height;
    let texture_aspect = (texture_width.max(1) as f32) / (texture_height.max(1) as f32);
    let camera_projector = scene
        .as_ref()
        .and_then(|scene| scene.camera_projector(texture_aspect));
    if let Some(scene_ref) = scene.as_ref() {
        if let Some(setup) = scene_ref.active_setup() {
            println!("[grim_viewer] active camera setup: {}", setup);
        }
        if let Some(camera) = scene_ref.camera.as_ref() {
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
    let texture_extent = wgpu::Extent3d {
        width: texture_width,
        height: texture_height,
        depth_or_array_layers: 1,
    };

    println!(
        "Preview texture sized {}x{} ({} bytes of source)",
        texture_width,
        texture_height,
        asset_bytes.len()
    );

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
    let texture_view = texture.create_view(&wgpu::TextureViewDescriptor::default());
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

    let upload = prepare_rgba_upload(texture_width, texture_height, &preview.data)?;
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
            rows_per_image: Some(texture_height),
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
                resource: wgpu::BindingResource::TextureView(&texture_view),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: wgpu::BindingResource::Sampler(&sampler),
            },
        ],
    });

    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("asset-shader"),
        source: wgpu::ShaderSource::Wgsl(Cow::Borrowed(SHADER_SOURCE)),
    });

    let layout_preset = layout_preset.unwrap_or_default();
    let audio_preset = layout_preset.audio.as_ref();
    let scrubber_preset = layout_preset.scrubber.as_ref();
    let timeline_preset = layout_preset.timeline.as_ref();
    let minimap_preset = layout_preset.minimap.as_ref();

    let audio_enabled =
        enable_audio_overlay && audio_preset.map(PanelPreset::enabled).unwrap_or(true);
    let (audio_overlay, audio_panel) = if audio_enabled {
        let config = OverlayConfig {
            width: 520,
            height: 144,
            padding_x: 8,
            padding_y: 8,
            label: "audio-overlay",
        }
        .with_preset(audio_preset);
        let panel = PanelSize::from(&config);
        let overlay = TextOverlay::new(&device, &queue, &bind_group_layout, size, config)?;
        (Some(overlay), Some(panel))
    } else {
        (None, None)
    };

    let scrubber = scene
        .as_ref()
        .and_then(|scene| MovementScrubber::new(scene));

    let scrubber_available = scrubber.is_some();
    let scrubber_enabled =
        scrubber_available && scrubber_preset.map(PanelPreset::enabled).unwrap_or(true);
    let (scrubber_overlay, scrubber_panel) = if scrubber_enabled {
        let config = OverlayConfig {
            width: 520,
            height: 176,
            padding_x: 8,
            padding_y: 8,
            label: "scrubber-overlay",
        }
        .with_preset(scrubber_preset);
        let panel = PanelSize::from(&config);
        let overlay = TextOverlay::new(&device, &queue, &bind_group_layout, size, config)?;
        (Some(overlay), Some(panel))
    } else {
        (None, None)
    };

    let timeline_available = scene
        .as_ref()
        .and_then(|scene| scene.timeline.as_ref())
        .is_some();

    let timeline_enabled =
        timeline_available && timeline_preset.map(PanelPreset::enabled).unwrap_or(true);
    let (timeline_overlay, timeline_panel) = if timeline_enabled {
        let config = OverlayConfig {
            width: 640,
            height: 224,
            padding_x: 8,
            padding_y: 8,
            label: "timeline-overlay",
        }
        .with_preset(timeline_preset);
        let panel = PanelSize::from(&config);
        let overlay = TextOverlay::new(&device, &queue, &bind_group_layout, size, config)?;
        (Some(overlay), Some(panel))
    } else {
        (None, None)
    };

    let mut minimap_constraints = MinimapConstraints::default();
    if let Some(preset) = minimap_preset {
        if let Some(min_side) = preset.min_side {
            minimap_constraints.min_side = min_side;
        }
        if let Some(preferred_fraction) = preset.preferred_fraction {
            minimap_constraints.preferred_fraction = preferred_fraction;
        }
        if let Some(max_fraction) = preset.max_fraction {
            minimap_constraints.max_fraction = max_fraction;
        }
    }
    let ui_layout = UiLayout::new(
        size,
        audio_panel,
        timeline_panel,
        scrubber_panel,
        minimap_constraints,
    )?;

    let quad_vertex_layout = wgpu::VertexBufferLayout {
        array_stride: std::mem::size_of::<QuadVertex>() as u64,
        step_mode: wgpu::VertexStepMode::Vertex,
        attributes: &wgpu::vertex_attr_array![0 => Float32x2, 1 => Float32x2],
    };

    let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("asset-pipeline-layout"),
        bind_group_layouts: &[&bind_group_layout],
        push_constant_ranges: &[],
    });

    let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
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

    let marker_vertex_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("marker-vertex-buffer"),
        contents: cast_slice(&MARKER_VERTICES),
        usage: wgpu::BufferUsages::VERTEX,
    });

    let initial_marker_capacity = 4usize;
    let marker_instance_buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("marker-instance-buffer"),
        size: (initial_marker_capacity * std::mem::size_of::<MarkerInstance>()) as u64,
        usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });

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
                format: wgpu::VertexFormat::Float32x3,
            },
        ],
    };

    let marker_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("marker-shader"),
        source: wgpu::ShaderSource::Wgsl(Cow::Borrowed(MARKER_SHADER_SOURCE)),
    });

    let marker_pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("marker-pipeline-layout"),
        bind_group_layouts: &[],
        push_constant_ranges: &[],
    });

    let marker_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("marker-pipeline"),
        layout: Some(&marker_pipeline_layout),
        vertex: wgpu::VertexState {
            module: &marker_shader,
            entry_point: "vs_main",
            buffers: &[marker_vertex_layout, marker_instance_layout],
            compilation_options: wgpu::PipelineCompilationOptions::default(),
        },
        fragment: Some(wgpu::FragmentState {
            module: &marker_shader,
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

    let selected_entity = scene.as_ref().and_then(|scene| {
        if scene.entities.is_empty() {
            None
        } else {
            Some(
                scene
                    .entities
                    .iter()
                    .enumerate()
                    .find(|(_, e)| e.position.is_some())
                    .map(|(idx, _)| idx)
                    .unwrap_or(0),
            )
        }
    });

    let mut state = ViewerState {
        window,
        surface,
        device,
        queue,
        config: wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format: surface_format,
            width: size.width.max(1),
            height: size.height.max(1),
            present_mode,
            alpha_mode,
            view_formats: vec![],
            desired_maximum_frame_latency: 1,
        },
        size,
        pipeline,
        quad_vertex_buffer,
        quad_index_buffer,
        quad_index_count,
        bind_group,
        _texture: texture,
        _texture_view: texture_view,
        _sampler: sampler,
        audio_overlay,
        timeline_overlay,
        scrubber_overlay,
        background,
        scene: scene.clone(),
        selected_entity,
        scrubber,
        camera_projector,
        marker_pipeline,
        marker_vertex_buffer,
        marker_instance_buffer,
        marker_capacity: initial_marker_capacity,
        ui_layout,
    };

    state.surface.configure(&state.device, &state.config);
    selection::print_selected_entity(&state);
    overlay_updates::refresh_timeline_overlay(&mut state);
    overlay_updates::refresh_scrubber_overlay(&mut state);
    layout::apply_panel_layouts(&mut state);

    Ok(state)
}
