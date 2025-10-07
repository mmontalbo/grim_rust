use super::super::markers::{
    DESK_ANCHOR_PALETTE, MANNY_ANCHOR_PALETTE, MARKER_VERTICES, MarkerInstance, MarkerProjection,
    TUBE_ANCHOR_PALETTE, entity_palette,
};
use super::super::overlays::TextOverlay;
use super::ViewerState;
use super::layout;
use bytemuck::cast_slice;
use wgpu::SurfaceError;

use crate::scene::{HotspotEventKind, SceneEntityKind, event_marker_style};

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

    {
        let mut rpass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("grim-viewer-pass"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &view,
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
        rpass.set_pipeline(&state.pipeline);
        rpass.set_bind_group(0, &state.bind_group, &[]);
        rpass.set_vertex_buffer(0, state.quad_vertex_buffer.slice(..));
        rpass.set_index_buffer(state.quad_index_buffer.slice(..), wgpu::IndexFormat::Uint16);
        rpass.draw_indexed(0..state.quad_index_count, 0, 0..1);
    }

    let marker_instances = build_marker_instances(state);
    if !marker_instances.is_empty() {
        ensure_marker_capacity(state, marker_instances.len());
        state.queue.write_buffer(
            &state.marker_instance_buffer,
            0,
            cast_slice(&marker_instances),
        );

        let mut marker_pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("marker-pass"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &view,
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
        marker_pass.set_pipeline(&state.marker_pipeline);
        marker_pass.set_vertex_buffer(0, state.marker_vertex_buffer.slice(..));
        let instance_byte_len =
            (marker_instances.len() * std::mem::size_of::<MarkerInstance>()) as u64;
        marker_pass.set_vertex_buffer(1, state.marker_instance_buffer.slice(0..instance_byte_len));
        marker_pass.draw(
            0..MARKER_VERTICES.len() as u32,
            0..marker_instances.len() as u32,
        );
    }

    if let Some(minimap_instances) = build_minimap_instances(state) {
        if !minimap_instances.is_empty() {
            ensure_marker_capacity(state, minimap_instances.len());
            state.queue.write_buffer(
                &state.marker_instance_buffer,
                0,
                cast_slice(&minimap_instances),
            );

            let mut minimap_pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("minimap-pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
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
            minimap_pass.set_pipeline(&state.marker_pipeline);
            minimap_pass.set_vertex_buffer(0, state.marker_vertex_buffer.slice(..));
            let minimap_byte_len =
                (minimap_instances.len() * std::mem::size_of::<MarkerInstance>()) as u64;
            minimap_pass
                .set_vertex_buffer(1, state.marker_instance_buffer.slice(0..minimap_byte_len));
            minimap_pass.draw(
                0..MARKER_VERTICES.len() as u32,
                0..minimap_instances.len() as u32,
            );
        }
    }

    if let Some(overlay) = state.audio_overlay.as_mut() {
        overlay.upload(&state.queue);
    }
    if let Some(overlay) = state.audio_overlay.as_ref() {
        draw_overlay(state, &view, overlay, "audio-overlay-pass", &mut encoder);
    }

    if let Some(overlay) = state.timeline_overlay.as_mut() {
        overlay.upload(&state.queue);
    }
    if let Some(overlay) = state.timeline_overlay.as_ref() {
        draw_overlay(state, &view, overlay, "timeline-overlay-pass", &mut encoder);
    }

    if let Some(overlay) = state.scrubber_overlay.as_mut() {
        overlay.upload(&state.queue);
    }
    if let Some(overlay) = state.scrubber_overlay.as_ref() {
        draw_overlay(state, &view, overlay, "scrubber-overlay-pass", &mut encoder);
    }

    state.queue.submit(std::iter::once(encoder.finish()));
    frame.present();
    Ok(())
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

fn build_marker_instances(state: &ViewerState) -> Vec<MarkerInstance> {
    let mut instances = Vec::new();

    let scene = match state.scene.as_ref() {
        Some(scene) => scene,
        None => return instances,
    };

    let projection = if let Some(projector) = state.camera_projector.as_ref() {
        MarkerProjection::Perspective(projector)
    } else {
        let bounds = match scene.position_bounds.as_ref() {
            Some(bounds) => bounds,
            None => return instances,
        };
        let (horizontal_axis, vertical_axis) = bounds.top_down_axes();
        let horizontal_min = bounds.min[horizontal_axis];
        let vertical_min = bounds.min[vertical_axis];
        let horizontal_span = (bounds.max[horizontal_axis] - horizontal_min).max(0.001);
        let vertical_span = (bounds.max[vertical_axis] - vertical_min).max(0.001);
        MarkerProjection::TopDown {
            horizontal_axis,
            vertical_axis,
            horizontal_min,
            vertical_min,
            horizontal_span,
            vertical_span,
        }
    };

    let selected = state.selected_entity;
    let mut push_marker = |position: [f32; 3], size: f32, color: [f32; 3], highlight: f32| {
        if let Some([ndc_x, ndc_y]) = projection.project(position) {
            if !ndc_x.is_finite() || !ndc_y.is_finite() {
                return;
            }
            instances.push(MarkerInstance {
                translate: [ndc_x, ndc_y],
                size,
                highlight,
                color,
                _padding: 0.0,
            });
        }
    };

    let mut scrub_position: Option<[f32; 3]> = None;
    if let Some(scrubber) = state.scrubber.as_ref() {
        if let Some(scene) = state.scene.as_ref() {
            if let Some(trace) = scene.movement_trace() {
                scrub_position = scrubber.current_position(trace);
            }
        }
    }

    if let Some(position) = scene.entity_position("manny") {
        let palette = MANNY_ANCHOR_PALETTE;
        push_marker(position, 0.1, palette.color, palette.highlight);
    }

    if let Some(position) = scene.entity_position("mo.computer") {
        let palette = DESK_ANCHOR_PALETTE;
        push_marker(position, 0.08, palette.color, palette.highlight);
    }

    if let Some(position) = scene
        .entity_position("mo.tube")
        .or_else(|| scene.entity_position("mo.tube.interest_actor"))
    {
        let palette = TUBE_ANCHOR_PALETTE;
        push_marker(position, 0.09, palette.color, palette.highlight);
    }

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
            base_size * 1.2
        } else {
            base_size
        };
        let palette = entity_palette(entity.kind, is_selected);
        push_marker(position, size, palette.color, palette.highlight);
    }

    if let (Some(scrub_position), Some(scene)) = (scrub_position, state.scene.as_ref()) {
        let palette = MANNY_ANCHOR_PALETTE;
        push_marker(
            scrub_position,
            0.11,
            palette.color,
            palette.highlight.max(0.9),
        );
        for (idx, event) in scene.hotspot_events().iter().enumerate() {
            if matches!(event.kind(), HotspotEventKind::Selection) {
                if Some(idx) == state.selected_entity {
                    push_marker(scrub_position, 0.12, [0.98, 0.93, 0.32], 0.95);
                }
            }
        }
    }

    instances
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
        size: layout.panel_width(),
        highlight: 0.0,
        color: [0.07, 0.08, 0.12],
        _padding: 0.0,
    });

    let mut push_marker = |position: [f32; 3], size: f32, color: [f32; 3], highlight: f32| {
        if let Some([ndc_x, ndc_y]) = projection.project(position) {
            if !ndc_x.is_finite() || !ndc_y.is_finite() {
                return;
            }
            instances.push(MarkerInstance {
                translate: [ndc_x, ndc_y],
                size,
                highlight,
                color,
                _padding: 0.0,
            });
        }
    };

    let scale_size = |base: f32| layout.scaled_size(base * 0.5);

    let mut scrub_position: Option<[f32; 3]> = None;
    let mut highlight_event_scene_index: Option<usize> = None;
    let mut desk_position = scene.entity_position("mo.computer");
    let mut tube_hint_position = scene.entity_position("mo.tube");

    if let Some(trace) = scene.movement_trace() {
        if !trace.samples.is_empty() {
            desk_position = trace.samples.first().map(|sample| sample.position);
            tube_hint_position = trace.samples.last().map(|sample| sample.position);

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
                push_marker(sample.position, path_size, path_color, 0.0);
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
                if Some(idx) == highlight_event_scene_index {
                    marker_highlight = marker_highlight.max(0.9);
                    marker_color = [0.98, 0.93, 0.32];
                    marker_size *= 1.08;
                }
                push_marker(
                    position,
                    scale_size(marker_size),
                    marker_color,
                    marker_highlight,
                );
            }
        }
    }

    let manny_position = scene.entity_position("manny");
    let manny_anchor = scrub_position.or(manny_position).or(desk_position);

    if let Some(position) = desk_position {
        let palette = DESK_ANCHOR_PALETTE;
        push_marker(
            position,
            scale_size(0.058),
            palette.color,
            palette.highlight,
        );
    }

    let tube_anchor = scene
        .entity_position("mo.tube")
        .or_else(|| scene.entity_position("mo.tube.interest_actor"))
        .or(tube_hint_position);

    if let Some(position) = tube_anchor {
        let palette = TUBE_ANCHOR_PALETTE;
        push_marker(
            position,
            scale_size(0.064),
            palette.color,
            palette.highlight,
        );
    }

    if let Some(position) = manny_anchor {
        let palette = MANNY_ANCHOR_PALETTE;
        push_marker(position, scale_size(0.07), palette.color, palette.highlight);
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
        push_marker(position, size, palette.color, palette.highlight);
    }

    Some(instances)
}

fn ensure_marker_capacity(state: &mut ViewerState, required: usize) {
    if required <= state.marker_capacity {
        return;
    }

    let new_capacity = required.next_power_of_two().max(4);
    let new_size = (new_capacity * std::mem::size_of::<MarkerInstance>()) as u64;
    state.marker_instance_buffer = state.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("marker-instance-buffer"),
        size: new_size,
        usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    state.marker_capacity = new_capacity;
}
