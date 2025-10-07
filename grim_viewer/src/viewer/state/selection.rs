use super::ViewerState;
use super::overlay_updates;

pub(super) fn next_entity(state: &mut ViewerState) {
    if let Some(scene) = state.scene.as_ref() {
        if scene.entities.is_empty() {
            return;
        }
        let next = match state.selected_entity {
            Some(idx) => (idx + 1) % scene.entities.len(),
            None => 0,
        };
        state.selected_entity = Some(next);
        print_selected_entity(state);
        overlay_updates::refresh_scene_overlays(state);
    }
}

pub(super) fn previous_entity(state: &mut ViewerState) {
    if let Some(scene) = state.scene.as_ref() {
        if scene.entities.is_empty() {
            return;
        }
        let prev = match state.selected_entity {
            Some(0) | None => scene.entities.len().saturating_sub(1),
            Some(idx) => idx.saturating_sub(1),
        };
        state.selected_entity = Some(prev);
        print_selected_entity(state);
        overlay_updates::refresh_scene_overlays(state);
    }
}

pub(super) fn handle_character_input(state: &mut ViewerState, key: &str) {
    match key {
        "]" => scrub_step(state, 1),
        "[" => scrub_step(state, -1),
        "}" => scrub_jump_to_head_target(state, 1),
        "{" => scrub_jump_to_head_target(state, -1),
        _ => {}
    }
}

pub(super) fn print_selected_entity(state: &ViewerState) {
    if let (Some(scene), Some(idx)) = (state.scene.as_ref(), state.selected_entity) {
        if let Some(entity) = scene.entities.get(idx) {
            println!("[grim_viewer] selected entity: {}", entity.describe());
            if let Some(position) = entity.position {
                println!(
                    "    position: ({:.3}, {:.3}, {:.3})",
                    position[0], position[1], position[2]
                );
            }
            if let Some(rotation) = entity.rotation {
                println!(
                    "    rotation: ({:.3}, {:.3}, {:.3})",
                    rotation[0], rotation[1], rotation[2]
                );
            }
            if let Some(target) = &entity.facing_target {
                println!("    facing target: {target}");
            }
            if let Some(control) = &entity.head_control {
                println!("    head control: {control}");
            }
            if let Some(rate) = entity.head_look_rate {
                println!("    head look rate: {rate:.3}");
            }
            if entity.last_played.is_some()
                || entity.last_looping.is_some()
                || entity.last_completed.is_some()
            {
                let played = entity.last_played.as_deref().unwrap_or("-");
                let looping = entity.last_looping.as_deref().unwrap_or("-");
                let completed = entity.last_completed.as_deref().unwrap_or("-");
                println!(
                    "    chore state: played={}, looping={}, completed={}",
                    played, looping, completed
                );
            }
            if entity.name.eq_ignore_ascii_case("manny") {
                if let Some(scene) = state.scene.as_ref() {
                    if let Some(trace) = scene.movement_trace() {
                        println!(
                            "    movement: {} samples (frames {}-{}) distance {:.3}",
                            trace.sample_count(),
                            trace.first_frame,
                            trace.last_frame,
                            trace.total_distance
                        );
                    }
                }
            }
        }
    }
}

fn scrub_step(state: &mut ViewerState, delta: i32) {
    if let Some(scrubber) = state.scrubber.as_mut() {
        let changed = scrubber.step(delta);
        if state.scrubber_overlay.is_some() || state.timeline_overlay.is_some() {
            overlay_updates::refresh_scene_overlays(state);
        }
        if changed {
            state.window().request_redraw();
        }
    }
}

fn scrub_jump_to_head_target(state: &mut ViewerState, direction: i32) {
    if let Some(scrubber) = state.scrubber.as_mut() {
        let changed = scrubber.jump_to_head_target(direction);
        if state.scrubber_overlay.is_some() || state.timeline_overlay.is_some() {
            overlay_updates::refresh_scene_overlays(state);
        }
        if changed {
            state.window().request_redraw();
        }
    }
}
