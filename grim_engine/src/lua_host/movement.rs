use std::fs;
use std::path::PathBuf;

use anyhow::{Context, Result};
use grim_stream::StateUpdate;
use mlua::{Function, Lua, Table, Value, Variadic};
use serde::Serialize;

use super::context::EngineContextHandle;
use super::types::Vec3;
use crate::stream::StreamServer;

const WALK_SPEED_SCALE: f32 = 0.009_999_999_78;

#[derive(Clone)]
pub struct MovementPlan {
    segments: Vec<MovementSegment>,
}

#[derive(Clone)]
struct MovementSegment {
    frames: u32,
    vector: Vec3,
}

impl MovementPlan {
    pub fn demo() -> Self {
        Self {
            segments: vec![
                MovementSegment {
                    frames: 36,
                    vector: Vec3 {
                        x: 0.0,
                        y: 2.0,
                        z: 0.0,
                    },
                },
                MovementSegment {
                    frames: 24,
                    vector: Vec3 {
                        x: 2.0,
                        y: 0.0,
                        z: 0.0,
                    },
                },
                MovementSegment {
                    frames: 24,
                    vector: Vec3 {
                        x: -2.0,
                        y: 0.0,
                        z: 0.0,
                    },
                },
                MovementSegment {
                    frames: 18,
                    vector: Vec3 {
                        x: 0.0,
                        y: -2.0,
                        z: 0.0,
                    },
                },
            ],
        }
    }
}

pub struct MovementOptions {
    plan: MovementPlan,
    log_path: Option<PathBuf>,
}

impl MovementOptions {
    pub fn demo(log_path: Option<PathBuf>) -> Self {
        Self {
            plan: MovementPlan::demo(),
            log_path,
        }
    }
}

#[derive(Serialize)]
pub(crate) struct MovementSample {
    pub(crate) frame: u32,
    pub(crate) position: [f32; 3],
    pub(crate) yaw: Option<f32>,
    pub(crate) sector: Option<String>,
}

pub(crate) fn simulate_movement(
    lua: &Lua,
    context: &EngineContextHandle,
    options: &MovementOptions,
    stream: Option<&StreamServer>,
) -> Result<()> {
    use anyhow::anyhow;

    let globals = lua.globals();
    let walk_vector: Table = globals
        .get("WalkVector")
        .context("WalkVector table missing for movement simulation")?;

    let (actor_handle, actor_id) = match context.resolve_actor_handle(&["manny", "Manny"]) {
        Some(pair) => pair,
        None => return Ok(()),
    };

    let mut frame: u32 = 0;
    let mut samples: Vec<MovementSample> = Vec::new();

    if let Ok(reset_controls) = globals.get::<_, Function>("ResetMarioControls") {
        let _: () = reset_controls.call(())?;
    }
    globals.set("MarioControl", true)?;
    if let Ok(system_table) = globals.get::<_, Table>("system") {
        let axis_stub = lua.create_function(|_, _: Variadic<Value>| Ok(()))?;
        system_table.set("axisHandler", axis_stub)?;
    }
    if let (Ok(single_start), Ok(walk_manny)) = (
        globals.get::<_, Function>("single_start_script"),
        globals.get::<_, Value>("WalkManny"),
    ) {
        let _: () = single_start.call((walk_manny,))?;
    }

    for segment in &options.plan.segments {
        for _ in 0..segment.frames {
            frame += 1;
            walk_vector.set("x", segment.vector.x)?;
            walk_vector.set("y", segment.vector.y)?;
            walk_vector.set("z", segment.vector.z)?;
            context
                .run_scripts(lua, 4, 32)
                .map_err(|err| anyhow!(err))?;

            if segment.vector.x.abs() + segment.vector.y.abs() + segment.vector.z.abs()
                > f32::EPSILON
            {
                let delta = Vec3 {
                    x: segment.vector.x * WALK_SPEED_SCALE,
                    y: segment.vector.y * WALK_SPEED_SCALE,
                    z: segment.vector.z * WALK_SPEED_SCALE,
                };
                context.walk_actor_vector(actor_handle, delta, None, None);
            }

            let sample_opt = capture_movement_sample(context, actor_handle, &actor_id, frame);
            if let Some(sample) = sample_opt {
                context.log_event(format!(
                    "movement.frame {} {:.3},{:.3}",
                    frame, sample.position[0], sample.position[1]
                ));
                if let Some(stream) = stream {
                    let update = StateUpdate {
                        seq: 0,
                        host_time_ns: 0,
                        frame: Some(sample.frame),
                        position: Some(sample.position),
                        yaw: sample.yaw,
                        active_setup: None,
                        active_hotspot: sample.sector.clone(),
                        coverage: Vec::new(),
                        events: Vec::new(),
                    };
                    if let Err(err) = stream.send_state_update(update) {
                        eprintln!("[grim_engine] failed to stream movement sample: {err:?}");
                    }
                }
                samples.push(sample);
            }
        }
    }

    walk_vector.set("x", 0.0)?;
    walk_vector.set("y", 0.0)?;
    walk_vector.set("z", 0.0)?;

    for _ in 0..12 {
        frame += 1;
        context
            .run_scripts(lua, 4, 32)
            .map_err(|err| anyhow!(err))?;
        context.walk_actor_vector(
            actor_handle,
            Vec3 {
                x: 0.0,
                y: 0.0,
                z: 0.0,
            },
            None,
            None,
        );
        let sample_opt = capture_movement_sample(context, actor_handle, &actor_id, frame);
        if let Some(sample) = sample_opt {
            context.log_event(format!(
                "movement.frame {} {:.3},{:.3}",
                frame, sample.position[0], sample.position[1]
            ));
            if let Some(stream) = stream {
                let update = StateUpdate {
                    seq: 0,
                    host_time_ns: 0,
                    frame: Some(sample.frame),
                    position: Some(sample.position),
                    yaw: sample.yaw,
                    active_setup: None,
                    active_hotspot: sample.sector.clone(),
                    coverage: Vec::new(),
                    events: Vec::new(),
                };
                if let Err(err) = stream.send_state_update(update) {
                    eprintln!("[grim_engine] failed to stream movement sample: {err:?}");
                }
            }
            samples.push(sample);
        }
    }

    if let Some(path) = options.log_path.as_ref() {
        let json =
            serde_json::to_string_pretty(&samples).context("serializing movement log to JSON")?;
        fs::write(path, json)
            .with_context(|| format!("writing movement log to {}", path.display()))?;
        println!("Saved movement log to {}", path.display());
    }

    Ok(())
}

pub(crate) fn capture_movement_sample(
    ctx: &EngineContextHandle,
    actor_handle: u32,
    actor_id: &str,
    frame: u32,
) -> Option<MovementSample> {
    let position = ctx.actor_position(actor_handle)?;
    let yaw = ctx.actor_rotation_y(actor_handle);
    let sector = ctx.geometry_sector_name(actor_id, "walk");
    Some(MovementSample {
        frame,
        position: [position.x, position.y, position.z],
        yaw,
        sector,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lua_host::context::{EngineContext, EngineContextHandle};
    use grim_analysis::resources::ResourceGraph;
    use std::cell::RefCell;
    use std::rc::Rc;

    #[test]
    fn demo_plan_contains_segments() {
        let plan = MovementPlan::demo();
        assert_eq!(plan.segments.len(), 4);
    }

    #[test]
    fn capture_sample_without_actor_returns_none() {
        let resources = Rc::new(ResourceGraph::default());
        let context = Rc::new(RefCell::new(EngineContext::new(
            resources, false, None, None,
        )));
        let handle = EngineContextHandle::new(context);
        let sample = capture_movement_sample(&handle, 9999, "manny", 0);
        assert!(sample.is_none());
    }
}
