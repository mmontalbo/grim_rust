use super::super::markers::{
    DESK_ANCHOR_PALETTE, MANNY_ANCHOR_PALETTE, MARKER_VERTICES, MarkerIcon, MarkerInstance,
    MarkerProjection, TUBE_ANCHOR_PALETTE, entity_palette,
};
use super::super::mesh::{
    MeshInstance, MeshUniforms, PrimitiveKind, bounds_scale, instance_transform,
    instance_transform_oriented, view_projection_uniform,
};
use super::super::overlays::TextOverlay;
use super::ViewerState;
use super::layout;
use bytemuck::cast_slice;
use glam::{Mat4, Quat, Vec3, Vec4};
use once_cell::sync::Lazy;
use std::{f32::consts::PI, sync::Mutex};
use wgpu::SurfaceError;

use crate::{
    scene::{
        CameraProjector, EntityOrientation, HotspotEventKind, SceneEntity, SceneEntityKind,
        event_marker_style,
    },
    ui_layout::{PanelKind, ViewportRect},
};

const MANNY_ANCHOR_SCALE: f32 = 1.35;
const DESK_ANCHOR_SCALE: f32 = 1.20;
const TUBE_ANCHOR_SCALE: f32 = 1.25;
const ENTITY_SCALE_BASE: f32 = 0.80;
const ENTITY_SCALE_SELECTED: f32 = 1.00;
const SELECTION_POINTER_SCALE: f32 = 0.60;
const SELECTION_POINTER_APEX_LIFT: f32 = 1.10;
const SELECTION_POINTER_CLEARANCE: f32 = 0.12;
const SELECTION_POINTER_COLOR: [f32; 3] = [0.98, 0.86, 0.32];
const SELECTION_POINTER_HIGHLIGHT: f32 = 1.0;
const AXIS_GIZMO_SCALE: f32 = 0.48;
const AXIS_GIZMO_MARGIN: f32 = 0.07;
const AXIS_GIZMO_DISTANCE: f32 = 16.0;
const AXIS_GIZMO_FORWARD_OFFSET: f32 = 0.65;
const AXIS_GIZMO_ORIGIN_RATIO: f32 = 0.35;
const AXIS_GIZMO_HIGHLIGHT: f32 = 0.78;
const AXIS_ORIGIN_HIGHLIGHT: f32 = 0.45;
const AXIS_X_COLOR: [f32; 3] = [0.94, 0.36, 0.3];
const AXIS_Y_COLOR: [f32; 3] = [0.32, 0.9, 0.52];
const AXIS_Z_COLOR: [f32; 3] = [0.32, 0.63, 0.95];
const AXIS_ORIGIN_COLOR: [f32; 3] = [0.84, 0.86, 0.92];

const SCALE_MIN: f32 = 0.045;
const SCALE_MAX: f32 = 3.5;

const HIGHLIGHT_BASE_BOOST: f32 = 0.75;
const HIGHLIGHT_RANGE: f32 = 0.45;
const COLOR_CLAMP_MIN: f32 = 0.05;
const COLOR_CLAMP_MAX: f32 = 1.0;
const HIGHLIGHT_BOOST_MIN: f32 = 0.4;
const HIGHLIGHT_BOOST_MAX: f32 = 1.6;
const PREVIEW_SPIN_RATE: f32 = 0.004;
/// Fraction of the preview viewport we try to occupy with the mesh's bounding box.
const PREVIEW_TARGET_EXTENT: f32 = 0.9;
const PREVIEW_ORBIT_FACTOR: f32 = 3.6;
const PREVIEW_ELEVATION_FACTOR: f32 = 2.4;
const PREVIEW_MIN_DISTANCE: f32 = 2.0;

static MANNY_SCALE_LOG: Lazy<Mutex<Option<(Option<f32>, Option<f32>, Option<f32>, f32)>>> =
    Lazy::new(|| Mutex::new(None));
static MANNY_POSITION_LOGGED: Lazy<Mutex<bool>> = Lazy::new(|| Mutex::new(false));

fn log_manny_scale_state(entity: &SceneEntity, base_scale: f32) {
    let actor_scale = entity.actor_scale;
    let collision_scale = entity.collision_scale;
    let viewer_scale = entity.scale_multiplier();
    let latest = Some((viewer_scale, actor_scale, collision_scale, base_scale));

    let mut last = MANNY_SCALE_LOG
        .lock()
        .expect("manny scale log mutex poisoned");
    if last.as_ref() != latest.as_ref() {
        let format_scale = |value: Option<f32>| match value {
            Some(scale) => format!("{scale:.3}"),
            None => String::from("-"),
        };
        log::info!(
            "[viewer] Manny scale resolved -> viewer={} actor={} collision={} base_scale={:.3}",
            format_scale(viewer_scale),
            format_scale(actor_scale),
            format_scale(collision_scale),
            base_scale
        );
        *last = latest;
    }
}

pub(super) fn render(state: &mut ViewerState) -> Result<(), SurfaceError> {
    let frame = state.surface.get_current_texture()?;
    let view = frame
        .texture
        .create_view(&wgpu::TextureViewDescriptor::default());
    let mut encoder = state
        .device
        .create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("grim-viewer-encoder"),
        });

    state.mesh_preview_angle = (state.mesh_preview_angle + PREVIEW_SPIN_RATE) % (2.0 * PI);

    draw_background(state, &view, &mut encoder);
    draw_scene_meshes(state, &view, &mut encoder);
    draw_mesh_preview(state, &view, &mut encoder);
    draw_minimap_markers(state, &view, &mut encoder);
    draw_overlays(state, &view, &mut encoder);

    state.queue.submit(std::iter::once(encoder.finish()));
    frame.present();
    Ok(())
}

fn draw_background(
    state: &mut ViewerState,
    view: &wgpu::TextureView,
    encoder: &mut wgpu::CommandEncoder,
) {
    let mut rpass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
        label: Some("grim-viewer-pass"),
        color_attachments: &[Some(wgpu::RenderPassColorAttachment {
            view,
            resolve_target: None,
            ops: wgpu::Operations {
                load: wgpu::LoadOp::Clear(state.background),
                store: wgpu::StoreOp::Store,
            },
        })],
        depth_stencil_attachment: None,
        timestamp_writes: None,
        occlusion_query_set: None,
    });
    let (vx, vy, vw, vh) = layout::plate_viewport(state);
    rpass.set_viewport(vx, vy, vw, vh, 0.0, 1.0);
    rpass.set_pipeline(&state.pipeline);
    rpass.set_bind_group(0, &state.bind_group, &[]);
    rpass.set_vertex_buffer(0, state.quad_vertex_buffer.slice(..));
    rpass.set_index_buffer(state.quad_index_buffer.slice(..), wgpu::IndexFormat::Uint16);
    rpass.draw_indexed(0..state.quad_index_count, 0, 0..1);
}

fn draw_scene_meshes(
    state: &mut ViewerState,
    view: &wgpu::TextureView,
    encoder: &mut wgpu::CommandEncoder,
) {
    if state.mesh.is_none() {
        return;
    }
    let Some(groups) = build_mesh_groups(state) else {
        return;
    };
    let total_instances = groups.total_instances();
    if total_instances == 0 {
        return;
    }
    let Some(camera) = state.camera_projector.as_ref() else {
        return;
    };
    let camera_matrix = camera.view_projection_matrix();

    ensure_mesh_instance_capacity(state, total_instances);

    let mesh = state.mesh.as_ref().expect("mesh resources present");

    let mut combined = Vec::with_capacity(total_instances);
    let manny_range = append_instances(&mut combined, &groups.manny);
    let sphere_range = append_instances(&mut combined, &groups.sphere);
    let cube_range = append_instances(&mut combined, &groups.cube);
    let cone_range = append_instances(&mut combined, &groups.cone);

    state
        .queue
        .write_buffer(&mesh.instance_buffer, 0, cast_slice(&combined));

    let uniform = view_projection_uniform(camera_matrix);
    state
        .queue
        .write_buffer(&mesh.uniform_buffer, 0, cast_slice(&[uniform]));

    let depth_attachment = wgpu::RenderPassDepthStencilAttachment {
        view: &mesh.depth_view,
        depth_ops: Some(wgpu::Operations {
            load: wgpu::LoadOp::Clear(1.0),
            store: wgpu::StoreOp::Store,
        }),
        stencil_ops: None,
    };

    let mut mesh_pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
        label: Some("mesh-pass"),
        color_attachments: &[Some(wgpu::RenderPassColorAttachment {
            view,
            resolve_target: None,
            ops: wgpu::Operations {
                load: wgpu::LoadOp::Load,
                store: wgpu::StoreOp::Store,
            },
        })],
        depth_stencil_attachment: Some(depth_attachment),
        timestamp_writes: None,
        occlusion_query_set: None,
    });

    let (vx, vy, vw, vh) = layout::plate_viewport(state);
    mesh_pass.set_viewport(vx, vy, vw, vh, 0.0, 1.0);
    mesh_pass.set_pipeline(&mesh.pipeline);
    mesh_pass.set_bind_group(0, &mesh.bind_group, &[]);

    let instance_bytes = (combined.len() * std::mem::size_of::<MeshInstance>()) as u64;
    mesh_pass.set_vertex_buffer(1, mesh.instance_buffer.slice(0..instance_bytes));

    if manny_range.count > 0 {
        if let Some(manny_mesh) = mesh.manny.as_ref() {
            mesh_pass.set_vertex_buffer(0, manny_mesh.buffers.vertex.slice(..));
            mesh_pass.set_index_buffer(
                manny_mesh.buffers.index.slice(..),
                wgpu::IndexFormat::Uint16,
            );
            mesh_pass.draw_indexed(
                0..manny_mesh.buffers.index_count,
                0,
                manny_range.offset..(manny_range.offset + manny_range.count),
            );
        }
    }

    if sphere_range.count > 0 {
        mesh_pass.set_vertex_buffer(0, mesh.sphere.vertex.slice(..));
        mesh_pass.set_index_buffer(mesh.sphere.index.slice(..), wgpu::IndexFormat::Uint16);
        mesh_pass.draw_indexed(
            0..mesh.sphere.index_count,
            0,
            sphere_range.offset..(sphere_range.offset + sphere_range.count),
        );
    }

    if cube_range.count > 0 {
        mesh_pass.set_vertex_buffer(0, mesh.cube.vertex.slice(..));
        mesh_pass.set_index_buffer(mesh.cube.index.slice(..), wgpu::IndexFormat::Uint16);
        mesh_pass.draw_indexed(
            0..mesh.cube.index_count,
            0,
            cube_range.offset..(cube_range.offset + cube_range.count),
        );
    }

    if cone_range.count > 0 {
        mesh_pass.set_vertex_buffer(0, mesh.cone.vertex.slice(..));
        mesh_pass.set_index_buffer(mesh.cone.index.slice(..), wgpu::IndexFormat::Uint16);
        mesh_pass.draw_indexed(
            0..mesh.cone.index_count,
            0,
            cone_range.offset..(cone_range.offset + cone_range.count),
        );
    }
}

fn draw_mesh_preview(
    state: &mut ViewerState,
    view: &wgpu::TextureView,
    encoder: &mut wgpu::CommandEncoder,
) {
    let Some(mesh_resources) = state.mesh.as_ref() else {
        return;
    };
    let Some(rect) = state
        .ui_layout
        .panel_rect(PanelKind::MeshPreview)
        .filter(|r| r.width > 4.0 && r.height > 4.0)
    else {
        return;
    };
    let Some(scene) = state.scene.as_ref() else {
        return;
    };
    let Some(selected_idx) = state.selected_entity else {
        return;
    };
    let Some(entity) = scene.entities.get(selected_idx) else {
        return;
    };
    if !entity.name.eq_ignore_ascii_case("manny") {
        return;
    }
    let Some(manny_mesh) = mesh_resources.manny.as_ref() else {
        return;
    };

    let preview = build_preview_draw(rect, state.mesh_preview_angle, manny_mesh);

    state.queue.write_buffer(
        &mesh_resources.preview.instance_buffer,
        0,
        cast_slice(&[preview.instance]),
    );
    state.queue.write_buffer(
        &mesh_resources.preview.uniform_buffer,
        0,
        cast_slice(&[preview.uniform]),
    );

    let depth_attachment = wgpu::RenderPassDepthStencilAttachment {
        view: &mesh_resources.depth_view,
        depth_ops: Some(wgpu::Operations {
            load: wgpu::LoadOp::Clear(1.0),
            store: wgpu::StoreOp::Store,
        }),
        stencil_ops: None,
    };

    let mut preview_pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
        label: Some("mesh-preview-pass"),
        color_attachments: &[Some(wgpu::RenderPassColorAttachment {
            view,
            resolve_target: None,
            ops: wgpu::Operations {
                load: wgpu::LoadOp::Load,
                store: wgpu::StoreOp::Store,
            },
        })],
        depth_stencil_attachment: Some(depth_attachment),
        timestamp_writes: None,
        occlusion_query_set: None,
    });
    preview_pass.set_pipeline(&mesh_resources.pipeline);
    preview_pass.set_bind_group(0, &mesh_resources.preview.bind_group, &[]);
    preview_pass.set_viewport(rect.x, rect.y, rect.width, rect.height, 0.0, 1.0);
    preview_pass.set_vertex_buffer(0, manny_mesh.buffers.vertex.slice(..));
    let instance_bytes = std::mem::size_of::<MeshInstance>() as u64;
    preview_pass.set_vertex_buffer(
        1,
        mesh_resources
            .preview
            .instance_buffer
            .slice(0..instance_bytes),
    );
    preview_pass.set_index_buffer(
        manny_mesh.buffers.index.slice(..),
        wgpu::IndexFormat::Uint16,
    );
    preview_pass.draw_indexed(0..manny_mesh.buffers.index_count, 0, 0..1);
}

fn draw_minimap_markers(
    state: &mut ViewerState,
    view: &wgpu::TextureView,
    encoder: &mut wgpu::CommandEncoder,
) {
    let Some(minimap_instances) = build_minimap_instances(state) else {
        return;
    };

    if minimap_instances.is_empty() {
        return;
    }

    ensure_minimap_marker_capacity(state, minimap_instances.len());
    state.queue.write_buffer(
        &state.minimap_marker_instance_buffer,
        0,
        cast_slice(&minimap_instances),
    );

    let mut minimap_pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
        label: Some("minimap-pass"),
        color_attachments: &[Some(wgpu::RenderPassColorAttachment {
            view,
            resolve_target: None,
            ops: wgpu::Operations {
                load: wgpu::LoadOp::Load,
                store: wgpu::StoreOp::Store,
            },
        })],
        depth_stencil_attachment: None,
        timestamp_writes: None,
        occlusion_query_set: None,
    });
    minimap_pass.set_pipeline(&state.minimap_pipeline);
    minimap_pass.set_vertex_buffer(0, state.minimap_marker_vertex_buffer.slice(..));
    let minimap_byte_len = (minimap_instances.len() * std::mem::size_of::<MarkerInstance>()) as u64;
    minimap_pass.set_vertex_buffer(
        1,
        state
            .minimap_marker_instance_buffer
            .slice(0..minimap_byte_len),
    );
    minimap_pass.draw(
        0..MARKER_VERTICES.len() as u32,
        0..minimap_instances.len() as u32,
    );
}

struct PreviewDraw {
    uniform: MeshUniforms,
    instance: MeshInstance,
}

fn build_preview_draw(rect: ViewportRect, angle: f32, manny: &super::MannyMesh) -> PreviewDraw {
    let max_extent = manny.max_half_extent.max(0.01);
    let scale = (PREVIEW_TARGET_EXTENT / max_extent).clamp(0.02, 50.0);
    let target_extent = max_extent * scale;

    let orbit_distance = (target_extent * PREVIEW_ORBIT_FACTOR).max(PREVIEW_MIN_DISTANCE);
    let eye = Vec3::new(
        orbit_distance * angle.cos(),
        orbit_distance * angle.sin(),
        target_extent * PREVIEW_ELEVATION_FACTOR,
    );
    let target = Vec3::ZERO;
    let up = Vec3::Z;

    let view_matrix = Mat4::look_at_rh(eye, target, up);
    let aspect = (rect.width / rect.height).max(0.1);
    let projection = Mat4::perspective_rh(32.0f32.to_radians(), aspect, 0.05, orbit_distance * 6.0);
    let uniform = view_projection_uniform(projection * view_matrix);

    let rotation = Mat4::from_quat(Quat::from_rotation_z(-angle));
    let scale_matrix = Mat4::from_scale(Vec3::splat(scale));
    let model = rotation * scale_matrix * manny.preview_center_matrix;

    PreviewDraw {
        uniform,
        instance: MeshInstance {
            model: model.to_cols_array_2d(),
            color: [0.92, 0.92, 0.92, 1.0],
        },
    }
}

fn draw_overlays(
    state: &mut ViewerState,
    view: &wgpu::TextureView,
    encoder: &mut wgpu::CommandEncoder,
) {
    state.overlays.upload_visible(&state.queue);

    if let Some(overlay) = state.overlays.audio() {
        draw_overlay(state, view, overlay, "audio-overlay-pass", encoder);
    }

    if let Some(overlay) = state.overlays.timeline() {
        draw_overlay(state, view, overlay, "timeline-overlay-pass", encoder);
    }

    if let Some(overlay) = state.overlays.scrubber() {
        draw_overlay(state, view, overlay, "scrubber-overlay-pass", encoder);
    }
}

fn draw_overlay(
    state: &ViewerState,
    view: &wgpu::TextureView,
    overlay: &TextOverlay,
    label: &'static str,
    encoder: &mut wgpu::CommandEncoder,
) {
    if !overlay.is_visible() {
        return;
    }
    let mut overlay_pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
        label: Some(label),
        color_attachments: &[Some(wgpu::RenderPassColorAttachment {
            view,
            resolve_target: None,
            ops: wgpu::Operations {
                load: wgpu::LoadOp::Load,
                store: wgpu::StoreOp::Store,
            },
        })],
        depth_stencil_attachment: None,
        timestamp_writes: None,
        occlusion_query_set: None,
    });
    overlay_pass.set_pipeline(&state.pipeline);
    overlay_pass.set_bind_group(0, overlay.bind_group(), &[]);
    overlay_pass.set_vertex_buffer(0, overlay.vertex_buffer().slice(..));
    overlay_pass.set_index_buffer(state.quad_index_buffer.slice(..), wgpu::IndexFormat::Uint16);
    overlay_pass.draw_indexed(0..state.quad_index_count, 0, 0..1);
}

/// Buckets mesh instances by primitive so each draw call stays compact.
#[derive(Default)]
struct MeshInstanceGroups {
    manny: Vec<MeshInstance>,
    sphere: Vec<MeshInstance>,
    cube: Vec<MeshInstance>,
    cone: Vec<MeshInstance>,
}

impl MeshInstanceGroups {
    fn total_instances(&self) -> usize {
        self.manny.len() + self.sphere.len() + self.cube.len() + self.cone.len()
    }

    fn push(&mut self, kind: PrimitiveKind, instance: MeshInstance) {
        match kind {
            PrimitiveKind::Sphere => self.sphere.push(instance),
            PrimitiveKind::Cube => self.cube.push(instance),
            PrimitiveKind::Cone => self.cone.push(instance),
        }
    }

    fn push_manny(&mut self, instance: MeshInstance) {
        self.manny.push(instance);
    }
}

#[derive(Clone, Copy, Default)]
struct InstanceRange {
    offset: u32,
    count: u32,
}

/// Append `source` instances into `target`, returning the range to draw.
fn append_instances(target: &mut Vec<MeshInstance>, source: &[MeshInstance]) -> InstanceRange {
    let offset = target.len() as u32;
    target.extend_from_slice(source);
    InstanceRange {
        offset,
        count: source.len() as u32,
    }
}

fn manny_mesh_instance(
    position: [f32; 3],
    orientation: Option<EntityOrientation>,
    base_scale: f32,
    mesh: &super::MannyMesh,
    color: [f32; 4],
    scale_multiplier: f32,
) -> MeshInstance {
    let clamped_multiplier = if scale_multiplier.is_finite() && scale_multiplier > 0.0 {
        scale_multiplier
    } else {
        1.0
    };
    let anchor_scale = scale_for_factor(base_scale, MANNY_ANCHOR_SCALE * clamped_multiplier);
    let radius = mesh.radius.unwrap_or(0.0).abs();
    let unit_scale = mesh.unit_scale.unwrap_or(1.0).abs().max(1e-4);
    let radius_world = radius * unit_scale;
    let mut scale = if radius_world > 1e-4 {
        anchor_scale / radius_world
    } else {
        anchor_scale
    };
    if !scale.is_finite() || scale <= 0.0 {
        scale = anchor_scale;
    }
    scale = scale.min(SCALE_MAX);
    let rotation = orientation
        .map(|basis| basis.quaternion)
        .unwrap_or(Quat::IDENTITY);
    let model = manny_model_matrix(position, rotation, scale, mesh.insert_offset);
    MeshInstance {
        model: model.to_cols_array_2d(),
        color,
    }
}

fn manny_model_matrix(
    position: [f32; 3],
    rotation: Quat,
    scale: f32,
    insert_offset: Option<[f32; 3]>,
) -> Mat4 {
    let translation = Mat4::from_translation(Vec3::from_array(position));
    let rotation = Mat4::from_quat(rotation);
    let scale = Mat4::from_scale(Vec3::splat(scale.max(1e-4)));
    let offset = insert_offset
        .map(|offset| Mat4::from_translation(-Vec3::from_array(offset)))
        .unwrap_or(Mat4::IDENTITY);
    translation * rotation * scale * offset
}

/// Grow the shared instance buffer if the current frame needs more slots.
fn ensure_mesh_instance_capacity(state: &mut ViewerState, required: usize) {
    let Some(mesh) = state.mesh.as_mut() else {
        return;
    };
    if required <= mesh.instance_capacity {
        return;
    }
    let mut capacity = mesh.instance_capacity.max(1);
    while capacity < required {
        capacity *= 2;
    }
    let new_size = (capacity * std::mem::size_of::<MeshInstance>()) as u64;
    let label = format!("mesh-instance-buffer({capacity})");
    let new_buffer = state.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some(label.as_str()),
        size: new_size,
        usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    mesh.instance_buffer = new_buffer;
    mesh.instance_capacity = capacity;
}

/// Collect per-entity mesh instances with sizes derived from the marker palette.
fn build_mesh_groups(state: &ViewerState) -> Option<MeshInstanceGroups> {
    let scene = state.scene.as_ref()?;
    let base_scale = bounds_scale(scene.position_bounds.as_ref());
    let camera = state.camera_projector.as_ref();
    let mut groups = MeshInstanceGroups::default();

    let manny_mesh = state
        .mesh
        .as_ref()
        .and_then(|resources| resources.manny.as_ref());
    let manny_mesh_loaded = manny_mesh.is_some();
    let manny_entity = scene
        .entities
        .iter()
        .find(|entity| entity.name.eq_ignore_ascii_case("manny"));

    if let Some(entity) = manny_entity {
        log_manny_scale_state(entity, base_scale);
        if let Some(position) = entity.position {
            if let Some(camera) = camera {
                let mut logged = MANNY_POSITION_LOGGED
                    .lock()
                    .expect("manny position log mutex poisoned");
                if !*logged {
                    let ndc = camera.project_ndc(position);
                    println!(
                        "[grim_viewer] Manny world position {:?}, ndc {:?}",
                        position,
                        ndc
                    );
                    *logged = true;
                }
            }
            let palette = MANNY_ANCHOR_PALETTE;
            let color = palette_to_color(palette.color, palette.highlight.max(0.6));
            let scale_multiplier = entity.scale_multiplier().unwrap_or(1.0);
            if let Some(mesh) = manny_mesh {
                let orientation = entity.orientation;
                let instance = manny_mesh_instance(
                    position,
                    orientation,
                    base_scale,
                    mesh,
                    color,
                    scale_multiplier,
                );
                groups.push_manny(instance);
            } else {
                groups.push(
                    PrimitiveKind::Sphere,
                    MeshInstance {
                        model: instance_transform(
                            position,
                            scale_for_factor(base_scale, MANNY_ANCHOR_SCALE * scale_multiplier),
                        ),
                        color,
                    },
                );
            }
        }
    }

    if let Some(entity) = scene
        .entities
        .iter()
        .find(|entity| entity.name.eq_ignore_ascii_case("mo.computer"))
    {
        if let Some(position) = entity.position {
            let palette = DESK_ANCHOR_PALETTE;
            let scale_multiplier = entity.scale_multiplier().unwrap_or(1.0);
            groups.push(
                PrimitiveKind::Cube,
                MeshInstance {
                    model: instance_transform_oriented(
                        position,
                        scale_for_factor(base_scale, DESK_ANCHOR_SCALE * scale_multiplier),
                        orientation_quat(entity.orientation),
                    ),
                    color: palette_to_color(palette.color, palette.highlight),
                },
            );
        }
    }

    if let Some(entity) = scene
        .entities
        .iter()
        .find(|entity| entity.name.eq_ignore_ascii_case("mo.tube.anchor"))
        .or_else(|| {
            scene
                .entities
                .iter()
                .find(|entity| entity.name.eq_ignore_ascii_case("mo.tube.interest_actor"))
        })
    {
        if let Some(position) = entity.position {
            let palette = TUBE_ANCHOR_PALETTE;
            let scale_multiplier = entity.scale_multiplier().unwrap_or(1.0);
            groups.push(
                PrimitiveKind::Cone,
                MeshInstance {
                    model: instance_transform_oriented(
                        position,
                        scale_for_factor(base_scale, TUBE_ANCHOR_SCALE * scale_multiplier),
                        orientation_quat(entity.orientation),
                    ),
                    color: palette_to_color(palette.color, palette.highlight),
                },
            );
        }
    }

    let pointer_rotation = Quat::from_rotation_arc(Vec3::Y, -Vec3::Z);

    for (idx, entity) in scene.entities.iter().enumerate() {
        let Some(position) = entity.position else {
            continue;
        };
        let is_selected = state.selected_entity == Some(idx);
        let base_palette = entity_palette(entity.kind, false);
        let scale_multiplier = entity.scale_multiplier().unwrap_or(1.0);
        let mut scale_factor = ENTITY_SCALE_BASE;
        if is_selected {
            scale_factor = ENTITY_SCALE_SELECTED;
        }
        let scale = scale_for_factor(base_scale, scale_factor * scale_multiplier);
        let highlight = if is_selected {
            base_palette.highlight.max(0.9)
        } else {
            base_palette.highlight
        };
        let skip_mesh = manny_mesh_loaded && entity.name.eq_ignore_ascii_case("manny");
        if !skip_mesh {
            // Keep each primitive tied to the scene entity's timeline category. The
            // cube/cone pairs for desk, cards, and tube all share transforms injected
            // by scene::manny::apply_geometry_overrides, so the proxies deliberately
            // overlap until decoded meshes replace them.
            groups.push(
                mesh_kind_for_entity(entity.kind),
                MeshInstance {
                    model: instance_transform_oriented(
                        position,
                        scale,
                        orientation_quat(entity.orientation),
                    ),
                    color: palette_to_color(base_palette.color, highlight),
                },
            );
        }

        if is_selected {
            let pointer_scale =
                scale_for_factor(base_scale, SELECTION_POINTER_SCALE * scale_multiplier);
            let apex_height = position[2]
                + pointer_scale * SELECTION_POINTER_APEX_LIFT
                + SELECTION_POINTER_CLEARANCE;
            let pointer_position = [position[0], position[1], apex_height + pointer_scale * 0.5];
            groups.push(
                PrimitiveKind::Cone,
                MeshInstance {
                    model: instance_transform_oriented(
                        pointer_position,
                        pointer_scale,
                        pointer_rotation,
                    ),
                    color: palette_to_color(SELECTION_POINTER_COLOR, SELECTION_POINTER_HIGHLIGHT),
                },
            );
        }
    }

    if let Some(camera) = state.camera_projector.as_ref() {
        push_axis_gizmo(&mut groups, camera, base_scale);
    }

    Some(groups)
}

fn mesh_kind_for_entity(kind: SceneEntityKind) -> PrimitiveKind {
    match kind {
        SceneEntityKind::Actor => PrimitiveKind::Sphere,
        SceneEntityKind::Object => PrimitiveKind::Cube,
        SceneEntityKind::InterestActor => PrimitiveKind::Cone,
    }
}

fn push_axis_gizmo(groups: &mut MeshInstanceGroups, camera: &CameraProjector, base_scale: f32) {
    let gizmo_scale = scale_for_factor(base_scale, AXIS_GIZMO_SCALE);
    if !gizmo_scale.is_finite() || gizmo_scale <= 0.0 {
        return;
    }

    let (right, up, forward) = camera.basis();
    let inv_view_proj = camera.view_projection_matrix().inverse();
    let margin = AXIS_GIZMO_MARGIN.clamp(0.0, 0.45);
    let forward_offset = gizmo_scale * AXIS_GIZMO_FORWARD_OFFSET;
    let gizmo_reach = camera.near_plane().max(0.2) + base_scale * AXIS_GIZMO_DISTANCE;
    let origin = gizmo_origin(camera, inv_view_proj, margin, forward_offset, gizmo_reach)
        .unwrap_or_else(|| {
            camera.position() + forward * gizmo_reach + right * gizmo_scale + up * gizmo_scale
        });

    let axes = [
        (Vec3::X, AXIS_X_COLOR),
        (Vec3::Y, AXIS_Y_COLOR),
        (Vec3::Z, AXIS_Z_COLOR),
    ];

    for (direction, color) in axes {
        let rotation = Quat::from_rotation_arc(Vec3::Y, direction);
        let translation = origin + direction * (gizmo_scale * 0.5);
        groups.push(
            PrimitiveKind::Cone,
            MeshInstance {
                model: instance_transform_oriented(translation.to_array(), gizmo_scale, rotation),
                color: palette_to_color(color, AXIS_GIZMO_HIGHLIGHT),
            },
        );
    }

    let origin_scale = gizmo_scale * AXIS_GIZMO_ORIGIN_RATIO;
    groups.push(
        PrimitiveKind::Sphere,
        MeshInstance {
            model: instance_transform(origin.to_array(), origin_scale),
            color: palette_to_color(AXIS_ORIGIN_COLOR, AXIS_ORIGIN_HIGHLIGHT),
        },
    );
}

fn gizmo_origin(
    camera: &CameraProjector,
    inverse_view_proj: Mat4,
    margin: f32,
    forward_offset: f32,
    reach: f32,
) -> Option<Vec3> {
    // Anchor the gizmo to the plate by unprojecting the top-right near-plane corner
    // and marching along that ray to a stable distance.
    let ndc_x = 1.0 - margin;
    let ndc_y = 1.0 - margin;
    let ndc_z = 0.0;
    let clip = Vec4::new(ndc_x, ndc_y, ndc_z * 2.0 - 1.0, 1.0);
    let world = inverse_view_proj * clip;
    if world.w.abs() <= f32::EPSILON {
        return None;
    }
    let corner = world.truncate() / world.w;
    if !corner.is_finite() {
        return None;
    }
    let camera_pos = camera.position();
    let direction = corner - camera_pos;
    if direction.length_squared() <= f32::EPSILON {
        return None;
    }
    let mut position = camera_pos + direction.normalize() * reach;
    let (_, _, forward) = camera.basis();
    position += forward * forward_offset;
    Some(position)
}

fn orientation_quat(orientation: Option<EntityOrientation>) -> Quat {
    orientation
        .map(|basis| basis.quaternion)
        .unwrap_or(Quat::IDENTITY)
}

/// Turn the 2D marker palette into a lit RGBA color for the mesh proxy.
fn palette_to_color(color: [f32; 3], highlight: f32) -> [f32; 4] {
    let boost = (HIGHLIGHT_BASE_BOOST + highlight * HIGHLIGHT_RANGE)
        .clamp(HIGHLIGHT_BOOST_MIN, HIGHLIGHT_BOOST_MAX);
    [
        (color[0] * boost).clamp(COLOR_CLAMP_MIN, COLOR_CLAMP_MAX),
        (color[1] * boost).clamp(COLOR_CLAMP_MIN, COLOR_CLAMP_MAX),
        (color[2] * boost).clamp(COLOR_CLAMP_MIN, COLOR_CLAMP_MAX),
        1.0,
    ]
}

/// Clamp the derived instance scale to keep proxies from dwarfing the plate.
fn scale_for_factor(base: f32, factor: f32) -> f32 {
    (base * factor).clamp(SCALE_MIN, SCALE_MAX)
}

fn build_minimap_instances(state: &ViewerState) -> Option<Vec<MarkerInstance>> {
    let scene = state.scene.as_ref()?;
    let bounds = scene.position_bounds.as_ref()?;
    let layout = layout::minimap_layout(state)?;

    let (horizontal_axis, vertical_axis) = bounds.top_down_axes();
    let horizontal_min = bounds.min[horizontal_axis];
    let vertical_min = bounds.min[vertical_axis];
    let horizontal_span = (bounds.max[horizontal_axis] - horizontal_min).max(0.001);
    let vertical_span = (bounds.max[vertical_axis] - vertical_min).max(0.001);

    let projection = MarkerProjection::TopDownPanel {
        horizontal_axis,
        vertical_axis,
        horizontal_min,
        vertical_min,
        horizontal_span,
        vertical_span,
        layout,
    };

    let mut instances = Vec::new();
    instances.push(MarkerInstance {
        translate: layout.center,
        depth: -0.95,
        size: layout.panel_width(),
        highlight: 0.0,
        color: [0.07, 0.08, 0.12],
        icon: MarkerIcon::Panel.id(),
    });

    let mut push_marker = |label: &str,
                           position: [f32; 3],
                           size: f32,
                           color: [f32; 3],
                           highlight: f32,
                           icon: MarkerIcon| {
        if let Some(instance) =
            projection.project_marker(Some(label), position, size, color, highlight, icon)
        {
            instances.push(instance);
        }
    };

    let scale_size = |base: f32| layout.scaled_size(base * 0.5);

    let mut scrub_position: Option<[f32; 3]> = None;
    let mut highlight_event_scene_index: Option<usize> = None;
    let mut desk_position = scene.entity_position("mo.computer");
    let mut tube_hint_position = scene.entity_position("mo.tube");

    if let Some(trace) = scene.movement_trace() {
        if !trace.samples.is_empty() {
            if desk_position.is_none() {
                desk_position = trace.samples.first().map(|sample| sample.position);
            }
            if tube_hint_position.is_none() {
                tube_hint_position = trace.samples.last().map(|sample| sample.position);
            }

            if let Some(scrubber) = state.scrubber.as_ref() {
                scrub_position = scrubber.current_position(trace);
                highlight_event_scene_index =
                    scrubber.highlighted_event().map(|event| event.scene_index);
            }

            let limit = 96_usize;
            let step = (trace.samples.len().max(limit) / limit).max(1);
            let path_color = [0.75, 0.65, 0.95];
            let path_size = scale_size(0.032);

            let len = trace.samples.len();
            for (idx, sample) in trace.samples.iter().enumerate().step_by(step) {
                if idx == 0 || idx + 1 == len {
                    continue;
                }
                push_marker(
                    "minimap.path",
                    sample.position,
                    path_size,
                    path_color,
                    0.0,
                    MarkerIcon::Path,
                );
            }

            for (idx, event) in scene.hotspot_events().iter().enumerate() {
                let frame = match event.frame {
                    Some(frame) => frame,
                    None => continue,
                };
                let position = match trace.nearest_position(frame) {
                    Some(pos) => pos,
                    None => continue,
                };
                let (mut marker_size, mut marker_color, mut marker_highlight) =
                    event_marker_style(event.kind());
                let mut marker_icon = event_marker_icon(event.kind());
                if Some(idx) == highlight_event_scene_index {
                    marker_highlight = marker_highlight.max(0.9);
                    marker_color = [0.98, 0.93, 0.32];
                    marker_size *= 1.08;
                    marker_icon = MarkerIcon::Ring;
                }
                push_marker(
                    event.kind().label(),
                    position,
                    scale_size(marker_size),
                    marker_color,
                    marker_highlight,
                    marker_icon,
                );
            }
        }
    }

    let manny_position = scene.entity_position("manny");
    let manny_anchor = scrub_position.or(manny_position).or(desk_position);

    if let Some(position) = desk_position {
        let palette = DESK_ANCHOR_PALETTE;
        push_marker(
            "desk-anchor",
            position,
            scale_size(0.058),
            palette.color,
            palette.highlight,
            palette.icon,
        );
    }

    let tube_anchor = scene
        .entity_position("mo.tube")
        .or_else(|| scene.entity_position("mo.tube.interest_actor"))
        .or(tube_hint_position);

    if let Some(position) = tube_anchor {
        let palette = TUBE_ANCHOR_PALETTE;
        push_marker(
            "tube-anchor",
            position,
            scale_size(0.064),
            palette.color,
            palette.highlight,
            palette.icon,
        );
    }

    if let Some(position) = manny_anchor {
        let palette = MANNY_ANCHOR_PALETTE;
        push_marker(
            "manny",
            position,
            scale_size(0.07),
            palette.color,
            palette.highlight,
            palette.icon,
        );
    }

    let selected = state.selected_entity;
    for (idx, entity) in scene.entities.iter().enumerate() {
        let position = match entity.position {
            Some(pos) => pos,
            None => continue,
        };

        let is_selected = matches!(selected, Some(sel) if sel == idx);
        let base_size = match entity.kind {
            SceneEntityKind::Actor => 0.06,
            SceneEntityKind::Object => 0.05,
            SceneEntityKind::InterestActor => 0.045,
        };
        let size = if is_selected {
            scale_size(base_size * 1.2)
        } else {
            scale_size(base_size)
        };
        let palette = entity_palette(entity.kind, is_selected);
        push_marker(
            entity.name.as_str(),
            position,
            size,
            palette.color,
            palette.highlight,
            palette.icon,
        );
    }

    Some(instances)
}

fn event_marker_icon(kind: HotspotEventKind) -> MarkerIcon {
    match kind {
        HotspotEventKind::Hotspot => MarkerIcon::Star,
        HotspotEventKind::HeadTarget => MarkerIcon::Path,
        HotspotEventKind::IgnoreBoxes => MarkerIcon::Square,
        HotspotEventKind::Chore => MarkerIcon::Diamond,
        HotspotEventKind::Dialog => MarkerIcon::Sphere,
        HotspotEventKind::Selection => MarkerIcon::Ring,
        HotspotEventKind::Other => MarkerIcon::Accent,
    }
}

fn ensure_minimap_marker_capacity(state: &mut ViewerState, required: usize) {
    if required <= state.minimap_marker_capacity {
        return;
    }

    let new_capacity = required.next_power_of_two().max(4);
    let new_size = (new_capacity * std::mem::size_of::<MarkerInstance>()) as u64;
    state.minimap_marker_instance_buffer = state.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("minimap-marker-instance-buffer"),
        size: new_size,
        usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    state.minimap_marker_capacity = new_capacity;
}
