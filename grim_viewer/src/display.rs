use anyhow::{Context, Result, anyhow};
use bytemuck::{Pod, Zeroable, cast_slice};
use std::fs::File;
use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{OnceLock, mpsc};
use wgpu::util::DeviceExt;
use winit::{dpi::PhysicalSize, window::Window};

use crate::layout::{Rect, ViewerLayout};
use crate::overlay::{TextOverlay, TextOverlayConfig};

pub struct ViewerState {
    window: std::sync::Arc<Window>,
    surface: wgpu::Surface<'static>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    config: wgpu::SurfaceConfiguration,
    size: PhysicalSize<u32>,
    pipeline: wgpu::RenderPipeline,
    index_buffer: wgpu::Buffer,
    index_count: u32,
    bind_group_layout: wgpu::BindGroupLayout,
    retail_bind_group: wgpu::BindGroup,
    retail_texture: wgpu::Texture,
    retail_texture_view: wgpu::TextureView,
    retail_sampler: wgpu::Sampler,
    retail_texture_size: (u32, u32),
    engine_bind_group: wgpu::BindGroup,
    _engine_texture: wgpu::Texture,
    _engine_texture_view: wgpu::TextureView,
    _engine_texture_size: (u32, u32),
    movie_renderer: MovieRenderer,
    frame_aspect: f32,
    layout: ViewerLayout,
    retail_rect: Rect,
    engine_rect: Rect,
    debug_rect: Rect,
    retail_label_rect: Rect,
    engine_label_rect: Rect,
    retail_vertex_buffer: wgpu::Buffer,
    engine_vertex_buffer: wgpu::Buffer,
    debug_vertex_buffer: wgpu::Buffer,
    retail_label_vertex_buffer: wgpu::Buffer,
    engine_label_vertex_buffer: wgpu::Buffer,
    debug_panel_overlay: TextOverlay,
    retail_label_overlay: TextOverlay,
    engine_label_overlay: TextOverlay,
    debug_lines: Vec<String>,
    frame_dump_done: bool,
}

struct PendingFrameDump {
    buffer: wgpu::Buffer,
    width: u32,
    height: u32,
    padded_bytes_per_row: u32,
    path: PathBuf,
}

static FRAME_DUMP_PATH: OnceLock<Option<PathBuf>> = OnceLock::new();
static MIRROR_MOVIE_TO_RETAIL: OnceLock<bool> = OnceLock::new();
static MIRRORED_MOVIE_FRAME: AtomicBool = AtomicBool::new(false);

impl ViewerState {
    pub async fn new(window: std::sync::Arc<Window>, width: u32, height: u32) -> Result<Self> {
        let instance = wgpu::Instance::default();
        let surface = instance.create_surface(window.clone())?;
        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::HighPerformance,
                force_fallback_adapter: false,
                compatible_surface: Some(&surface),
            })
            .await
            .ok_or_else(|| anyhow!("no suitable GPU adapter found"))?;

        let (device, queue) = adapter
            .request_device(
                &wgpu::DeviceDescriptor {
                    label: Some("grim-viewer-device"),
                    required_features: wgpu::Features::empty(),
                    required_limits: wgpu::Limits::default(),
                },
                None,
            )
            .await?;

        let surface_caps = surface.get_capabilities(&adapter);
        let surface_format = surface_caps
            .formats
            .iter()
            .copied()
            .find(|format| format.is_srgb())
            .unwrap_or(surface_caps.formats[0]);

        let window_size = window.inner_size();
        let mut state = Self::create(
            window,
            surface,
            device,
            queue,
            surface_format,
            window_size,
            width.max(1),
            height.max(1),
        )?;
        state.configure_surface();
        Ok(state)
    }

    pub fn window(&self) -> &Window {
        &self.window
    }

    pub fn window_handle(&self) -> std::sync::Arc<Window> {
        self.window.clone()
    }

    pub fn enable_next_frame_dump(&mut self) {
        self.frame_dump_done = false;
    }

    pub fn size(&self) -> PhysicalSize<u32> {
        self.size
    }

    pub fn resize(&mut self, new_size: PhysicalSize<u32>) {
        if new_size.width > 0 && new_size.height > 0 {
            self.size = new_size;
            self.config.width = new_size.width;
            self.config.height = new_size.height;
            self.configure_surface();
            if let Err(err) = self.update_layout() {
                eprintln!("[grim_viewer] layout update failed after resize: {err:?}");
            }
        }
    }

    pub fn render(&mut self) -> Result<(), wgpu::SurfaceError> {
        self.debug_panel_overlay.upload(&self.queue);
        self.retail_label_overlay.upload(&self.queue);
        self.engine_label_overlay.upload(&self.queue);

        let frame = self.surface.get_current_texture()?;
        let view = frame
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("grim-viewer-encoder"),
            });

        {
            let mut render_pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("grim-viewer-pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });

            render_pass.set_pipeline(&self.pipeline);
            render_pass.set_index_buffer(self.index_buffer.slice(..), wgpu::IndexFormat::Uint16);

            render_pass.set_vertex_buffer(0, self.retail_vertex_buffer.slice(..));
            render_pass.set_bind_group(0, &self.retail_bind_group, &[]);
            render_pass.draw_indexed(0..self.index_count, 0, 0..1);

            render_pass.set_vertex_buffer(0, self.engine_vertex_buffer.slice(..));
            let movie_visible = self.movie_renderer.is_visible();
            println!(
                "[grim_viewer] render movie_visible={movie_visible} rect={:?}",
                self.engine_rect
            );
            if movie_visible {
                self.movie_renderer.log_draw();
                render_pass.set_bind_group(0, self.movie_renderer.bind_group(), &[]);
            } else {
                render_pass.set_bind_group(0, &self.engine_bind_group, &[]);
            }
            render_pass.draw_indexed(0..self.index_count, 0, 0..1);

            if self.debug_panel_overlay.is_visible() {
                render_pass.set_vertex_buffer(0, self.debug_vertex_buffer.slice(..));
                render_pass.set_bind_group(0, self.debug_panel_overlay.bind_group(), &[]);
                render_pass.draw_indexed(0..self.index_count, 0, 0..1);
            }

            if self.retail_label_overlay.is_visible() {
                render_pass.set_vertex_buffer(0, self.retail_label_vertex_buffer.slice(..));
                render_pass.set_bind_group(0, self.retail_label_overlay.bind_group(), &[]);
                render_pass.draw_indexed(0..self.index_count, 0, 0..1);
            }

            if self.engine_label_overlay.is_visible() {
                render_pass.set_vertex_buffer(0, self.engine_label_vertex_buffer.slice(..));
                render_pass.set_bind_group(0, self.engine_label_overlay.bind_group(), &[]);
                render_pass.draw_indexed(0..self.index_count, 0, 0..1);
            }
        }

        let pending_dump = self.maybe_prepare_frame_dump(&frame, &mut encoder);
        self.queue.submit(std::iter::once(encoder.finish()));
        if let Some(dump) = pending_dump {
            if let Err(err) = self.finish_frame_dump(dump) {
                eprintln!("[grim_viewer] failed to dump frame: {err:?}");
            }
        }
        frame.present();
        Ok(())
    }

    fn maybe_prepare_frame_dump(
        &mut self,
        frame: &wgpu::SurfaceTexture,
        encoder: &mut wgpu::CommandEncoder,
    ) -> Option<PendingFrameDump> {
        if self.frame_dump_done {
            return None;
        }
        let Some(path) = frame_dump_path() else {
            return None;
        };
        let width = self.config.width;
        let height = self.config.height;
        if width == 0 || height == 0 {
            return None;
        }
        let bytes_per_pixel = 4u32;
        let unpadded_row_bytes = width.saturating_mul(bytes_per_pixel);
        let padded_row_bytes = align_to(unpadded_row_bytes, wgpu::COPY_BYTES_PER_ROW_ALIGNMENT);
        let buffer_size = padded_row_bytes as u64 * height as u64;
        let buffer = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("grim-viewer-frame-dump"),
            size: buffer_size,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        encoder.copy_texture_to_buffer(
            wgpu::ImageCopyTexture {
                texture: &frame.texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::ImageCopyBuffer {
                buffer: &buffer,
                layout: wgpu::ImageDataLayout {
                    offset: 0,
                    bytes_per_row: Some(padded_row_bytes),
                    rows_per_image: Some(height),
                },
            },
            wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
        );
        self.frame_dump_done = true;
        Some(PendingFrameDump {
            buffer,
            width,
            height,
            padded_bytes_per_row: padded_row_bytes,
            path,
        })
    }

    fn finish_frame_dump(&self, dump: PendingFrameDump) -> Result<()> {
        let slice = dump.buffer.slice(..);
        let (sender, receiver) = mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |result| {
            let _ = sender.send(result);
        });
        self.device.poll(wgpu::Maintain::Wait);
        let map_result = receiver
            .recv()
            .context("frame dump buffer map channel closed")?;
        map_result.context("frame dump buffer map failed")?;

        let data = slice.get_mapped_range();
        let mut file = File::create(&dump.path)
            .with_context(|| anyhow!("failed to create frame dump at {}", dump.path.display()))?;
        let header = format!("P6\n{} {}\n255\n", dump.width, dump.height);
        file.write_all(header.as_bytes())
            .context("failed to write frame dump header")?;

        let row_bytes = dump.width as usize * 4;
        let padded_row = dump.padded_bytes_per_row as usize;
        for row in 0..dump.height as usize {
            let offset = row * padded_row;
            let row_slice = &data[offset..offset + row_bytes];
            for pixel in row_slice.chunks_exact(4) {
                file.write_all(&pixel[..3])
                    .context("failed to write frame dump pixels")?;
            }
        }
        drop(data);
        dump.buffer.unmap();
        println!(
            "[grim_viewer] frame dump written to {}",
            dump.path.display()
        );
        Ok(())
    }

    pub fn upload_frame(
        &mut self,
        width: u32,
        height: u32,
        stride_bytes: u32,
        data: &[u8],
    ) -> Result<()> {
        if width == 0 || height == 0 {
            return Ok(());
        }

        if (width, height) != self.retail_texture_size {
            self.recreate_retail_texture(width, height)?;
        }

        let row_bytes = width
            .checked_mul(4)
            .ok_or_else(|| anyhow!("frame width overflow"))? as usize;
        let stride = if stride_bytes == 0 {
            row_bytes
        } else {
            stride_bytes as usize
        };
        if stride < row_bytes {
            return Err(anyhow!(
                "stride {stride} smaller than row bytes {row_bytes}"
            ));
        }
        if data.len() < stride * height as usize {
            return Err(anyhow!(
                "frame data {} smaller than stride {} * height {}",
                data.len(),
                stride,
                height
            ));
        }

        let upload = if stride == row_bytes {
            FrameUpload::Borrowed(&data[..row_bytes * height as usize])
        } else {
            FrameUpload::Owned(repack_rows(row_bytes, stride, height as usize, data))
        };

        let extent = wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        };

        self.queue.write_texture(
            wgpu::ImageCopyTexture {
                texture: &self.retail_texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            upload.bytes(),
            wgpu::ImageDataLayout {
                offset: 0,
                bytes_per_row: Some(row_bytes as u32),
                rows_per_image: Some(height),
            },
            extent,
        );
        Ok(())
    }

    #[allow(dead_code)]
    pub fn upload_engine_frame(&mut self, width: u32, height: u32, data: &[u8]) -> Result<()> {
        if width == 0 || height == 0 {
            return Ok(());
        }

        let expected_len = width
            .checked_mul(height)
            .and_then(|pixels| pixels.checked_mul(4))
            .ok_or_else(|| anyhow!("engine frame dimensions overflow: {}x{}", width, height))?
            as usize;

        if data.len() < expected_len {
            return Err(anyhow!(
                "engine frame data {} smaller than expected {} ({}x{})",
                data.len(),
                expected_len,
                width,
                height
            ));
        }

        if (width, height) != self._engine_texture_size {
            self.recreate_engine_texture(width, height)?;
        }

        self.queue.write_texture(
            wgpu::ImageCopyTexture {
                texture: &self._engine_texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            &data[..expected_len],
            wgpu::ImageDataLayout {
                offset: 0,
                bytes_per_row: Some(width.saturating_mul(4)),
                rows_per_image: Some(height),
            },
            wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
        );
        Ok(())
    }

    pub fn upload_movie_frame(
        &mut self,
        width: u32,
        height: u32,
        stride_bytes: u32,
        data: &[u8],
    ) -> Result<()> {
        self.movie_renderer.upload_frame(
            &self.device,
            &self.queue,
            &self.bind_group_layout,
            &self.retail_sampler,
            width,
            height,
            stride_bytes,
            data,
        )?;

        if mirror_movie_to_retail_enabled() && !MIRRORED_MOVIE_FRAME.swap(true, Ordering::SeqCst) {
            if let Err(err) = self.debug_mirror_movie_to_retail(width, height, stride_bytes, data) {
                eprintln!("[grim_viewer] failed to mirror movie frame into retail pane: {err:?}");
            }
        }

        Ok(())
    }

    pub fn hide_movie(&mut self) {
        self.movie_renderer.hide();
        MIRRORED_MOVIE_FRAME.store(false, Ordering::SeqCst);
    }

    pub fn set_frame_dimensions(&mut self, width: u32, height: u32) {
        if width == 0 || height == 0 {
            return;
        }
        let aspect = (width as f32) / (height as f32);
        if (aspect - self.frame_aspect).abs() > 0.001 {
            self.frame_aspect = aspect;
            if let Err(err) = self.update_layout() {
                eprintln!("[grim_viewer] layout update failed for new frame config: {err:?}");
            }
        }
    }

    pub fn set_debug_lines(&mut self, lines: &[String]) {
        self.debug_lines = lines.to_vec();
        self.debug_panel_overlay.set_lines(&self.debug_lines);
    }

    pub fn set_retail_label(&mut self, text: &str) {
        self.retail_label_overlay.set_label(text);
    }

    pub fn set_engine_label(&mut self, text: &str) {
        self.engine_label_overlay.set_label(text);
    }

    fn debug_mirror_movie_to_retail(
        &mut self,
        width: u32,
        height: u32,
        stride_bytes: u32,
        data: &[u8],
    ) -> Result<()> {
        println!(
            "[grim_viewer] mirroring movie frame into retail pane ({}x{}, stride={})",
            width, height, stride_bytes
        );
        self.upload_frame(width, height, stride_bytes, data)
    }

    fn update_layout(&mut self) -> Result<()> {
        self.layout = ViewerLayout::compute(self.size, self.frame_aspect);
        self.retail_rect = self.layout.retail_view;
        self.engine_rect = self.layout.engine_view;
        self.debug_rect = self.layout.debug_panel;

        self.retail_vertex_buffer =
            create_vertex_buffer_for_rect(&self.device, self.size, self.retail_rect, "retail-view");
        self.engine_vertex_buffer =
            create_vertex_buffer_for_rect(&self.device, self.size, self.engine_rect, "engine-view");
        self.debug_vertex_buffer =
            create_vertex_buffer_for_rect(&self.device, self.size, self.debug_rect, "debug-panel");

        let debug_width = self.debug_rect.width.round().max(64.0) as u32;
        let debug_height = self.debug_rect.height.round().max(64.0) as u32;
        self.debug_panel_overlay.resize(
            &self.device,
            &self.queue,
            &self.bind_group_layout,
            debug_width,
            debug_height,
        )?;
        self.debug_panel_overlay.set_lines(&self.debug_lines);

        self.retail_label_rect = label_rect_for(
            self.retail_rect,
            self.retail_label_overlay.size(),
            self.size,
        );
        self.engine_label_rect = label_rect_for(
            self.engine_rect,
            self.engine_label_overlay.size(),
            self.size,
        );
        self.retail_label_vertex_buffer = create_vertex_buffer_for_rect(
            &self.device,
            self.size,
            self.retail_label_rect,
            "retail-label",
        );
        self.engine_label_vertex_buffer = create_vertex_buffer_for_rect(
            &self.device,
            self.size,
            self.engine_label_rect,
            "engine-label",
        );

        Ok(())
    }

    fn create(
        window: std::sync::Arc<Window>,
        surface: wgpu::Surface<'static>,
        device: wgpu::Device,
        queue: wgpu::Queue,
        format: wgpu::TextureFormat,
        window_size: PhysicalSize<u32>,
        texture_width: u32,
        texture_height: u32,
    ) -> Result<Self> {
        let config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            format,
            width: window_size.width.max(1),
            height: window_size.height.max(1),
            present_mode: wgpu::PresentMode::Fifo,
            desired_maximum_frame_latency: 2,
            alpha_mode: wgpu::CompositeAlphaMode::Opaque,
            view_formats: vec![],
        };

        let (retail_texture, retail_texture_view) =
            create_texture(&device, texture_width, texture_height)?;
        let retail_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("grim-viewer-sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::FilterMode::Nearest,
            ..Default::default()
        });

        let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("grim-viewer-bind-group-layout"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        multisampled: false,
                        view_dimension: wgpu::TextureViewDimension::D2,
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
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

        let retail_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("grim-viewer-retail-bind-group"),
            layout: &bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&retail_texture_view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&retail_sampler),
                },
            ],
        });

        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("grim-viewer-shader"),
            source: wgpu::ShaderSource::Wgsl(SHADER.into()),
        });

        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("grim-viewer-pipeline-layout"),
            bind_group_layouts: &[&bind_group_layout],
            push_constant_ranges: &[],
        });

        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("grim-viewer-pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: "vs_main",
                buffers: &[wgpu::VertexBufferLayout {
                    array_stride: std::mem::size_of::<Vertex>() as u64,
                    step_mode: wgpu::VertexStepMode::Vertex,
                    attributes: &wgpu::vertex_attr_array![0 => Float32x2, 1 => Float32x2],
                }],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: "fs_main",
                targets: &[Some(wgpu::ColorTargetState {
                    format,
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

        let index_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("grim-viewer-indices"),
            contents: bytemuck::cast_slice(INDICES),
            usage: wgpu::BufferUsages::INDEX,
        });
        let index_count = INDICES.len() as u32;

        let frame_aspect = if texture_height == 0 {
            16.0 / 9.0
        } else {
            (texture_width.max(1) as f32) / (texture_height.max(1) as f32)
        };
        let layout = ViewerLayout::compute(window_size, frame_aspect);

        let retail_vertex_buffer =
            create_vertex_buffer_for_rect(&device, window_size, layout.retail_view, "retail-view");
        let engine_vertex_buffer =
            create_vertex_buffer_for_rect(&device, window_size, layout.engine_view, "engine-view");
        let debug_vertex_buffer =
            create_vertex_buffer_for_rect(&device, window_size, layout.debug_panel, "debug-panel");

        let (engine_texture, engine_texture_view) = create_texture(&device, 1, 1)?;
        queue.write_texture(
            wgpu::ImageCopyTexture {
                texture: &engine_texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            &[32, 36, 44, 255],
            wgpu::ImageDataLayout {
                offset: 0,
                bytes_per_row: Some(4),
                rows_per_image: Some(1),
            },
            wgpu::Extent3d {
                width: 1,
                height: 1,
                depth_or_array_layers: 1,
            },
        );
        let engine_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("grim-viewer-engine-bind-group"),
            layout: &bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&engine_texture_view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&retail_sampler),
                },
            ],
        });

        let debug_panel_config = TextOverlayConfig::new(
            layout.debug_panel.width.round().max(64.0) as u32,
            layout.debug_panel.height.round().max(64.0) as u32,
            16,
            12,
            "debug-panel",
        );
        let mut debug_panel_overlay =
            TextOverlay::new(&device, &queue, &bind_group_layout, debug_panel_config)?;
        debug_panel_overlay.set_lines(&[]);
        debug_panel_overlay.upload(&queue);

        let mut retail_label_overlay = TextOverlay::new(
            &device,
            &queue,
            &bind_group_layout,
            TextOverlayConfig::new(320, 32, 12, 8, "retail-label"),
        )?;
        retail_label_overlay.set_label("Retail Capture");
        retail_label_overlay.upload(&queue);

        let mut engine_label_overlay = TextOverlay::new(
            &device,
            &queue,
            &bind_group_layout,
            TextOverlayConfig::new(320, 32, 12, 8, "engine-label"),
        )?;
        engine_label_overlay.set_label("Rust Engine");
        engine_label_overlay.upload(&queue);

        let retail_label_rect =
            label_rect_for(layout.retail_view, retail_label_overlay.size(), window_size);
        let engine_label_rect =
            label_rect_for(layout.engine_view, engine_label_overlay.size(), window_size);

        let retail_label_vertex_buffer =
            create_vertex_buffer_for_rect(&device, window_size, retail_label_rect, "retail-label");
        let engine_label_vertex_buffer =
            create_vertex_buffer_for_rect(&device, window_size, engine_label_rect, "engine-label");
        let movie_renderer = MovieRenderer::new(&device, &bind_group_layout, &retail_sampler)?;

        Ok(Self {
            window,
            surface,
            device,
            queue,
            config,
            size: window_size,
            pipeline,
            index_buffer,
            index_count,
            bind_group_layout,
            retail_bind_group,
            retail_texture,
            retail_texture_view,
            retail_sampler,
            retail_texture_size: (texture_width, texture_height),
            engine_bind_group,
            _engine_texture: engine_texture,
            _engine_texture_view: engine_texture_view,
            _engine_texture_size: (1, 1),
            movie_renderer,
            frame_aspect,
            layout,
            retail_rect: layout.retail_view,
            engine_rect: layout.engine_view,
            debug_rect: layout.debug_panel,
            retail_label_rect,
            engine_label_rect,
            retail_vertex_buffer,
            engine_vertex_buffer,
            debug_vertex_buffer,
            retail_label_vertex_buffer,
            engine_label_vertex_buffer,
            debug_panel_overlay,
            retail_label_overlay,
            engine_label_overlay,
            debug_lines: Vec::new(),
            frame_dump_done: false,
        })
    }

    fn recreate_retail_texture(&mut self, width: u32, height: u32) -> Result<()> {
        let (texture, texture_view) = create_texture(&self.device, width, height)?;
        self.retail_texture = texture;
        self.retail_texture_view = texture_view;
        self.retail_texture_size = (width, height);

        self.retail_bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("grim-viewer-retail-bind-group"),
            layout: &self.bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&self.retail_texture_view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&self.retail_sampler),
                },
            ],
        });
        Ok(())
    }

    #[allow(dead_code)]
    fn recreate_engine_texture(&mut self, width: u32, height: u32) -> Result<()> {
        let (texture, texture_view) = create_texture(&self.device, width, height)?;
        self._engine_texture = texture;
        self._engine_texture_view = texture_view;
        self._engine_texture_size = (width, height);

        self.engine_bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("grim-viewer-engine-bind-group"),
            layout: &self.bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&self._engine_texture_view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&self.retail_sampler),
                },
            ],
        });
        Ok(())
    }

    fn configure_surface(&mut self) {
        self.surface.configure(&self.device, &self.config);
    }
}

fn create_texture(
    device: &wgpu::Device,
    width: u32,
    height: u32,
) -> Result<(wgpu::Texture, wgpu::TextureView)> {
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("grim-viewer-texture"),
        size: wgpu::Extent3d {
            width: width.max(1),
            height: height.max(1),
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8UnormSrgb,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
    Ok((texture, view))
}

fn create_vertex_buffer_for_rect(
    device: &wgpu::Device,
    window: PhysicalSize<u32>,
    rect: Rect,
    label: &str,
) -> wgpu::Buffer {
    let vertices = rect_vertices(rect, window);
    device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some(label),
        contents: cast_slice(&vertices),
        usage: wgpu::BufferUsages::VERTEX,
    })
}

fn rect_vertices(rect: Rect, window: PhysicalSize<u32>) -> [Vertex; 4] {
    let width = window.width.max(1) as f32;
    let height = window.height.max(1) as f32;

    let left = (rect.x / width) * 2.0 - 1.0;
    let right = ((rect.x + rect.width) / width) * 2.0 - 1.0;
    let top = 1.0 - (rect.y / height) * 2.0;
    let bottom = 1.0 - ((rect.y + rect.height) / height) * 2.0;

    [
        Vertex {
            position: [left, top],
            uv: [0.0, 0.0],
        },
        Vertex {
            position: [right, top],
            uv: [1.0, 0.0],
        },
        Vertex {
            position: [right, bottom],
            uv: [1.0, 1.0],
        },
        Vertex {
            position: [left, bottom],
            uv: [0.0, 1.0],
        },
    ]
}

fn label_rect_for(view: Rect, overlay_size: (u32, u32), window: PhysicalSize<u32>) -> Rect {
    let width = overlay_size.0.max(1) as f32;
    let height = overlay_size.1.max(1) as f32;
    let gap = 8.0;
    let mut x = view.x;
    let max_x = window.width.max(1) as f32 - width - gap;
    if x > max_x {
        x = max_x.max(gap);
    }
    let mut y = view.y - height - gap;
    if y < gap {
        y = gap;
    }
    Rect {
        x,
        y,
        width,
        height,
    }
}

fn repack_rows(row_bytes: usize, stride: usize, rows: usize, data: &[u8]) -> Vec<u8> {
    let mut output = vec![0u8; row_bytes * rows];
    for row in 0..rows {
        let src_offset = row * stride;
        let dst_offset = row * row_bytes;
        let end = src_offset + row_bytes;
        output[dst_offset..dst_offset + row_bytes].copy_from_slice(&data[src_offset..end]);
    }
    output
}

fn frame_dump_path() -> Option<PathBuf> {
    FRAME_DUMP_PATH
        .get_or_init(|| std::env::var_os("GRIM_DUMP_FRAME").map(PathBuf::from))
        .clone()
}

fn mirror_movie_to_retail_enabled() -> bool {
    *MIRROR_MOVIE_TO_RETAIL.get_or_init(|| {
        matches!(
            std::env::var("GRIM_MOVIE_MIRROR_RETAIL")
                .unwrap_or_default()
                .to_ascii_lowercase()
                .as_str(),
            "1" | "true" | "yes"
        )
    })
}

fn align_to(value: u32, alignment: u32) -> u32 {
    if alignment == 0 {
        return value;
    }
    ((value + alignment - 1) / alignment) * alignment
}

struct MovieRenderer {
    texture: wgpu::Texture,
    view: wgpu::TextureView,
    bind_group: wgpu::BindGroup,
    size: (u32, u32),
    visible: bool,
    debug_draw_logged: bool,
    debug_logs_remaining: u32,
}

impl MovieRenderer {
    fn new(
        device: &wgpu::Device,
        layout: &wgpu::BindGroupLayout,
        sampler: &wgpu::Sampler,
    ) -> Result<Self> {
        let (texture, view) = create_texture(device, 1, 1)?;
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("grim-viewer-movie-bind-group"),
            layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(sampler),
                },
            ],
        });
        Ok(Self {
            texture,
            view,
            bind_group,
            size: (1, 1),
            visible: false,
            debug_draw_logged: false,
            debug_logs_remaining: 5,
        })
    }

    fn upload_frame(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        layout: &wgpu::BindGroupLayout,
        sampler: &wgpu::Sampler,
        width: u32,
        height: u32,
        stride_bytes: u32,
        data: &[u8],
    ) -> Result<()> {
        let was_visible = self.visible;
        if width == 0 || height == 0 {
            self.visible = false;
            return Ok(());
        }

        if (width, height) != self.size {
            self.recreate_texture(device, layout, sampler, width, height)?;
        }

        let row_bytes = width
            .checked_mul(4)
            .ok_or_else(|| anyhow!("movie frame width overflow: {}", width))?
            as usize;
        let stride = if stride_bytes == 0 {
            row_bytes
        } else {
            stride_bytes as usize
        };
        if stride < row_bytes {
            return Err(anyhow!(
                "movie stride {stride} smaller than row bytes {row_bytes}"
            ));
        }
        if data.len() < stride * height as usize {
            return Err(anyhow!(
                "movie frame data {} smaller than expected {}",
                data.len(),
                stride * height as usize
            ));
        }

        let upload = if stride == row_bytes {
            FrameUpload::Borrowed(&data[..row_bytes * height as usize])
        } else {
            FrameUpload::Owned(repack_rows(row_bytes, stride, height as usize, data))
        };

        if self.debug_logs_remaining > 0 {
            let bytes = upload.bytes();
            let mut sample = Vec::new();
            for chunk in bytes.chunks_exact(4).take(4) {
                sample.push(format!("{:?}", chunk));
            }
            let center_offset = (height as usize / 2) * row_bytes + (width as usize / 2) * 4;
            let center_pixel = &bytes[center_offset..center_offset + 4];
            println!(
                "[grim_viewer] movie frame sample pixels {} center {:?}",
                sample.join(" "),
                center_pixel
            );
            if center_pixel[..3] != [0, 0, 0] {
                self.debug_logs_remaining = 0;
            } else {
                self.debug_logs_remaining = self.debug_logs_remaining.saturating_sub(1);
            }
        }

        queue.write_texture(
            wgpu::ImageCopyTexture {
                texture: &self.texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            upload.bytes(),
            wgpu::ImageDataLayout {
                offset: 0,
                bytes_per_row: Some(row_bytes as u32),
                rows_per_image: Some(height),
            },
            wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
        );
        self.visible = true;
        self.debug_draw_logged = false;
        if !was_visible {
            println!(
                "[grim_viewer] movie renderer visible ({}x{}, stride={})",
                width, height, stride_bytes
            );
        }
        println!(
            "[grim_viewer] movie frame uploaded ({}x{}, stride={})",
            width, height, stride_bytes
        );
        Ok(())
    }

    fn recreate_texture(
        &mut self,
        device: &wgpu::Device,
        layout: &wgpu::BindGroupLayout,
        sampler: &wgpu::Sampler,
        width: u32,
        height: u32,
    ) -> Result<()> {
        let (texture, view) = create_texture(device, width, height)?;
        self.texture = texture;
        self.view = view;
        self.size = (width, height);
        self.bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("grim-viewer-movie-bind-group"),
            layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&self.view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(sampler),
                },
            ],
        });
        self.debug_logs_remaining = 5;
        Ok(())
    }

    fn is_visible(&self) -> bool {
        self.visible
    }

    fn hide(&mut self) {
        self.visible = false;
        self.debug_draw_logged = false;
        self.debug_logs_remaining = 5;
        println!("[grim_viewer] movie renderer hidden");
    }

    fn bind_group(&self) -> &wgpu::BindGroup {
        &self.bind_group
    }

    fn log_draw(&mut self) {
        if !self.debug_draw_logged {
            println!(
                "[grim_viewer] movie renderer binding {}x{} texture",
                self.size.0, self.size.1
            );
            self.debug_draw_logged = true;
        }
    }
}

enum FrameUpload<'a> {
    Borrowed(&'a [u8]),
    Owned(Vec<u8>),
}

impl<'a> FrameUpload<'a> {
    fn bytes(&self) -> &[u8] {
        match self {
            FrameUpload::Borrowed(slice) => slice,
            FrameUpload::Owned(vec) => vec,
        }
    }
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct Vertex {
    position: [f32; 2],
    uv: [f32; 2],
}

const INDICES: &[u16] = &[0, 1, 2, 0, 2, 3];

const SHADER: &str = r#"
struct VertexOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn vs_main(@location(0) position: vec2<f32>, @location(1) uv: vec2<f32>) -> VertexOutput {
    var out: VertexOutput;
    out.position = vec4<f32>(position, 0.0, 1.0);
    out.uv = uv;
    return out;
}

@group(0) @binding(0)
var texture_data: texture_2d<f32>;

@group(0) @binding(1)
var texture_sampler: sampler;

@fragment
fn fs_main(input: VertexOutput) -> @location(0) vec4<f32> {
    return textureSample(texture_data, texture_sampler, input.uv);
}
"#;
