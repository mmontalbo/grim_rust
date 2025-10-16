use anyhow::{Result, anyhow};
use bytemuck::{Pod, Zeroable};
use wgpu::util::DeviceExt;
use winit::{dpi::PhysicalSize, window::Window};

pub struct ViewerState {
    window: std::sync::Arc<Window>,
    surface: wgpu::Surface<'static>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    config: wgpu::SurfaceConfiguration,
    size: PhysicalSize<u32>,
    pipeline: wgpu::RenderPipeline,
    vertex_buffer: wgpu::Buffer,
    index_buffer: wgpu::Buffer,
    index_count: u32,
    bind_group_layout: wgpu::BindGroupLayout,
    bind_group: wgpu::BindGroup,
    texture: wgpu::Texture,
    texture_view: wgpu::TextureView,
    sampler: wgpu::Sampler,
    texture_size: (u32, u32),
}

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

    pub fn size(&self) -> PhysicalSize<u32> {
        self.size
    }

    pub fn resize(&mut self, new_size: PhysicalSize<u32>) {
        if new_size.width > 0 && new_size.height > 0 {
            self.size = new_size;
            self.config.width = new_size.width;
            self.config.height = new_size.height;
            self.configure_surface();
        }
    }

    pub fn render(&mut self) -> Result<(), wgpu::SurfaceError> {
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
            render_pass.set_bind_group(0, &self.bind_group, &[]);
            render_pass.set_vertex_buffer(0, self.vertex_buffer.slice(..));
            render_pass.set_index_buffer(self.index_buffer.slice(..), wgpu::IndexFormat::Uint16);
            render_pass.draw_indexed(0..self.index_count, 0, 0..1);
        }

        self.queue.submit(std::iter::once(encoder.finish()));
        frame.present();
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

        if (width, height) != self.texture_size {
            self.recreate_texture(width, height)?;
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
            extent,
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
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format,
            width: window_size.width.max(1),
            height: window_size.height.max(1),
            present_mode: wgpu::PresentMode::Fifo,
            desired_maximum_frame_latency: 2,
            alpha_mode: wgpu::CompositeAlphaMode::Opaque,
            view_formats: vec![],
        };

        let (texture, texture_view) = create_texture(&device, texture_width, texture_height)?;
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
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

        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("grim-viewer-bind-group"),
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
                    blend: Some(wgpu::BlendState::REPLACE),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            }),
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview: None,
        });

        let vertex_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("grim-viewer-vertices"),
            contents: bytemuck::cast_slice(VERTICES),
            usage: wgpu::BufferUsages::VERTEX,
        });

        let index_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("grim-viewer-indices"),
            contents: bytemuck::cast_slice(INDICES),
            usage: wgpu::BufferUsages::INDEX,
        });

        Ok(Self {
            window,
            surface,
            device,
            queue,
            config,
            size: window_size,
            pipeline,
            vertex_buffer,
            index_buffer,
            index_count: INDICES.len() as u32,
            bind_group_layout,
            bind_group,
            texture,
            texture_view,
            sampler,
            texture_size: (texture_width, texture_height),
        })
    }

    fn recreate_texture(&mut self, width: u32, height: u32) -> Result<()> {
        let (texture, texture_view) = create_texture(&self.device, width, height)?;
        self.texture = texture;
        self.texture_view = texture_view;
        self.texture_size = (width, height);

        self.bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("grim-viewer-bind-group"),
            layout: &self.bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&self.texture_view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&self.sampler),
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

const VERTICES: &[Vertex] = &[
    Vertex {
        position: [-1.0, -1.0],
        uv: [0.0, 1.0],
    },
    Vertex {
        position: [-1.0, 1.0],
        uv: [0.0, 0.0],
    },
    Vertex {
        position: [1.0, 1.0],
        uv: [1.0, 0.0],
    },
    Vertex {
        position: [1.0, -1.0],
        uv: [1.0, 1.0],
    },
];

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
