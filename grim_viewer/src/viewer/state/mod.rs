//! Central runtime state for the viewer. Owns the wgpu device/surface,
//! maintains overlay text, minimap markers, and entity selection, and exposes
//! small helpers that the event loop in `main.rs` drives. Submodules cover
//! lifecycle slices: `init` for setup, `layout` for resize handling,
//! `overlay_updates` for text refresh, `render` for draw passes, and
//! `selection` for input routing.

use std::sync::Arc;

use crate::audio::AudioStatus;
use crate::cli::LayoutPreset;
use crate::scene::{CameraProjector, MovementScrubber, ViewerScene};
use crate::ui_layout::UiLayout;
use anyhow::Result;
use wgpu::SurfaceError;
use winit::{dpi::PhysicalSize, window::Window};

pub struct ViewerState {
    window: Arc<Window>,
    surface: wgpu::Surface<'static>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    config: wgpu::SurfaceConfiguration,
    size: PhysicalSize<u32>,
    pipeline: wgpu::RenderPipeline,
    quad_vertex_buffer: wgpu::Buffer,
    quad_index_buffer: wgpu::Buffer,
    quad_index_count: u32,
    bind_group: wgpu::BindGroup,
    _texture: wgpu::Texture,
    _texture_view: wgpu::TextureView,
    _sampler: wgpu::Sampler,
    overlays: panels::ViewerOverlays,
    background: wgpu::Color,
    texture_size: PhysicalSize<u32>,
    scene: Option<Arc<ViewerScene>>,
    selected_entity: Option<usize>,
    scrubber: Option<MovementScrubber>,
    camera_projector: Option<CameraProjector>,
    marker_pipeline: wgpu::RenderPipeline,
    minimap_pipeline: wgpu::RenderPipeline,
    scene_marker_vertex_buffer: wgpu::Buffer,
    scene_marker_instance_buffer: wgpu::Buffer,
    scene_marker_capacity: usize,
    minimap_marker_vertex_buffer: wgpu::Buffer,
    minimap_marker_instance_buffer: wgpu::Buffer,
    minimap_marker_capacity: usize,
    mesh: Option<MeshResources>,
    ui_layout: UiLayout,
}

mod init;
mod layout;
mod overlay_updates;
mod panels;
mod render;
mod selection;

impl ViewerState {
    pub async fn new(
        window: Arc<Window>,
        asset_name: &str,
        asset_bytes: Vec<u8>,
        decode_result: Result<crate::texture::PreviewTexture>,
        scene: Option<Arc<ViewerScene>>,
        enable_audio_overlay: bool,
        layout_preset: Option<LayoutPreset>,
    ) -> Result<Self> {
        init::new(
            window,
            asset_name,
            asset_bytes,
            decode_result,
            scene,
            enable_audio_overlay,
            layout_preset,
        )
        .await
    }

    pub fn window(&self) -> &Window {
        self.window.as_ref()
    }

    pub fn size(&self) -> PhysicalSize<u32> {
        self.size
    }

    pub fn resize(&mut self, new_size: PhysicalSize<u32>) {
        layout::resize(self, new_size);
    }

    pub fn render(&mut self) -> Result<(), SurfaceError> {
        render::render(self)
    }

    pub fn update_audio_overlay(&mut self, status: &AudioStatus) {
        overlay_updates::update_audio_overlay(self, status);
    }

    pub fn next_entity(&mut self) {
        selection::next_entity(self);
    }

    pub fn previous_entity(&mut self) {
        selection::previous_entity(self);
    }

    pub fn handle_key_event(&mut self, event: &winit::event::KeyEvent) {
        selection::handle_key_event(self, event);
    }

    pub(super) fn rebuild_mesh_depth(&mut self) {
        if let Some(mesh) = self.mesh.as_mut() {
            let (texture, view) = init::create_mesh_depth_texture(&self.device, self.size);
            mesh.depth_texture = texture;
            mesh.depth_view = view;
        }
    }
}

pub(super) struct MeshResources {
    pub pipeline: wgpu::RenderPipeline,
    pub bind_group: wgpu::BindGroup,
    pub uniform_buffer: wgpu::Buffer,
    pub depth_texture: wgpu::Texture,
    pub depth_view: wgpu::TextureView,
    pub instance_buffer: wgpu::Buffer,
    pub instance_capacity: usize,
    pub sphere: PrimitiveBuffers,
    pub cube: PrimitiveBuffers,
    pub cone: PrimitiveBuffers,
}

pub(super) struct PrimitiveBuffers {
    pub vertex: wgpu::Buffer,
    pub index: wgpu::Buffer,
    pub index_count: u32,
}
