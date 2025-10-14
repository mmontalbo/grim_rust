use anyhow::{anyhow, Context, Result};
use mlua::{Function, Lua, Table, Value};

use super::context::EngineContextHandle;
use super::movement::capture_movement_sample;
use super::types::{Vec3, MANNY_OFFICE_SEED_POS};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum HotspotSlug {
    Computer,
}

impl HotspotSlug {
    fn from_str(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "computer" => Some(HotspotSlug::Computer),
            _ => None,
        }
    }

    fn label(&self) -> &'static str {
        match self {
            HotspotSlug::Computer => "computer",
        }
    }

    fn object_field(&self) -> &'static str {
        match self {
            HotspotSlug::Computer => "computer",
        }
    }

    fn approach_target(&self) -> Vec3 {
        match self {
            HotspotSlug::Computer => Vec3 {
                x: 0.5,
                y: 1.975,
                z: 0.0,
            },
        }
    }

    fn approach_steps(&self) -> u32 {
        match self {
            HotspotSlug::Computer => 24,
        }
    }
}

#[derive(Clone)]
pub struct HotspotOptions {
    slug: HotspotSlug,
}

impl HotspotOptions {
    pub fn parse(value: &str) -> Result<Self> {
        let slug = HotspotSlug::from_str(value)
            .ok_or_else(|| anyhow!("unknown hotspot demo: {}", value))?;
        Ok(Self { slug })
    }

    fn slug(&self) -> HotspotSlug {
        self.slug
    }
}

pub(crate) fn simulate_hotspot_demo(
    lua: &Lua,
    context: &EngineContextHandle,
    options: &HotspotOptions,
) -> Result<()> {
    let slug = options.slug();

    let (actor_handle, actor_id) = match context.resolve_actor_handle(&["manny", "Manny"]) {
        Some(pair) => pair,
        None => return Ok(()),
    };

    let target = slug.approach_target();
    let steps = slug.approach_steps().max(1);

    context.log_event(format!("hotspot.demo.approach {}", slug.label()));

    let mut frame: u32 = 0;
    for step in 0..steps {
        frame += 1;
        let current = context
            .actor_position(actor_handle)
            .unwrap_or(MANNY_OFFICE_SEED_POS);
        let remaining = (steps - step) as f32;
        let delta = Vec3 {
            x: (target.x - current.x) / remaining.max(1.0),
            y: (target.y - current.y) / remaining.max(1.0),
            z: (target.z - current.z) / remaining.max(1.0),
        };

        context.walk_actor_vector(actor_handle, delta, None, None);
        context
            .run_scripts(lua, 4, 32)
            .map_err(|err| anyhow!(err))?;

        if let Some(sample) = capture_movement_sample(context, actor_handle, &actor_id, frame) {
            context.log_event(format!(
                "movement.frame {} {:.3},{:.3}",
                frame, sample.position[0], sample.position[1]
            ));
        }
    }

    let globals = lua.globals();
    let mo_table: Table = globals
        .get("mo")
        .context("mo table missing for hotspot demo")?;
    let object: Table = mo_table
        .get(slug.object_field())
        .with_context(|| format!("mo.{} missing for hotspot demo", slug.object_field()))?;
    let object_clone = object.clone();
    let sentence: Function = globals
        .get("Sentence")
        .context("Sentence function missing for hotspot demo")?;

    context.log_event(format!("hotspot.demo.start {}", slug.label()));

    sentence
        .call::<_, ()>(("use", object.clone()))
        .context("executing hotspot Sentence")?;

    for _ in 0..32 {
        context
            .run_scripts(lua, 64, 4096)
            .map_err(|err| anyhow!(err))?;
        let costume_reset = context
            .actor_costume("manny")
            .map(|costume| costume.eq_ignore_ascii_case("suit"))
            .unwrap_or(true);
        let message_idle = !context.is_message_active();
        if costume_reset && message_idle {
            break;
        }
    }
    context
        .run_scripts(lua, 32, 2048)
        .map_err(|err| anyhow!(err))?;

    let fallback_needed = context
        .actor_costume("manny")
        .map(|costume| !costume.eq_ignore_ascii_case("suit"))
        .unwrap_or(false);

    if fallback_needed {
        complete_computer_hotspot_manually(lua, context, object_clone)?;
    }

    context.log_event(format!("hotspot.demo.end {}", slug.label()));

    Ok(())
}

fn complete_computer_hotspot_manually(
    lua: &Lua,
    context: &EngineContextHandle,
    target: Table,
) -> Result<()> {
    let globals = lua.globals();
    let start_sfx: Function = globals
        .get("start_sfx")
        .context("start_sfx not available for hotspot fallback")?;
    let stop_sound: Function = globals
        .get("stop_sound")
        .context("stop_sound not available for hotspot fallback")?;
    let wait_for_sound: Function = globals
        .get("wait_for_sound")
        .context("wait_for_sound not available for hotspot fallback")?;
    let enable_head_control: Function = globals
        .get("enable_head_control")
        .context("enable_head_control not available for hotspot fallback")?;

    let manny: Table = globals
        .get("manny")
        .context("manny table missing for hotspot fallback")?;
    let say_line: Function = manny
        .get("say_line")
        .context("manny.say_line missing for hotspot fallback")?;
    let wait_for_message: Function = manny
        .get("wait_for_message")
        .context("manny.wait_for_message missing for hotspot fallback")?;
    let head_look_at: Function = manny
        .get("head_look_at")
        .context("manny.head_look_at missing for hotspot fallback")?;
    let head_look_at_point: Function = manny
        .get("head_look_at_point")
        .context("manny.head_look_at_point missing for hotspot fallback")?;
    let play_chore: Function = manny
        .get("play_chore")
        .context("manny.play_chore missing for hotspot fallback")?;
    let pop_costume: Function = manny
        .get("pop_costume")
        .context("manny.pop_costume missing for hotspot fallback")?;
    let set_pos: Function = manny
        .get("setpos")
        .context("manny.setpos missing for hotspot fallback")?;
    let set_rot: Function = manny
        .get("setrot")
        .context("manny.setrot missing for hotspot fallback")?;
    let ignore_boxes: Function = manny
        .get("ignore_boxes")
        .context("manny.ignore_boxes missing for hotspot fallback")?;

    let _ = stop_sound.call::<_, ()>(("keyboard.imu",));

    start_sfx.call::<_, Value>(("txtScrl3.WAV",))?;
    let _ = wait_for_sound.call::<_, ()>(("txtScrl3.WAV",));
    start_sfx.call::<_, Value>(("txtScrl2.WAV",))?;
    start_sfx.call::<_, Value>(("compbeep.wav",))?;
    head_look_at_point.call::<_, ()>((manny.clone(), 0.20_f32, 1.875_f32, 0.47_f32, 90_f32))?;
    say_line.call::<_, ()>((manny.clone(), "/moma112/"))?;
    wait_for_message.call::<_, ()>((manny.clone(),))?;
    head_look_at.call::<_, ()>((manny.clone(), target.clone()))?;
    say_line.call::<_, ()>((manny.clone(), "/moma113/"))?;

    play_chore.call::<_, ()>((manny.clone(), "ma_note_type_type_loop", "ma_note_type.cos"))?;
    start_sfx.call::<_, Value>(("keyboard.imu",))?;
    start_sfx.call::<_, Value>(("txtScrl3.WAV",))?;
    let _ = wait_for_sound.call::<_, ()>(("txtScrl3.WAV",));
    start_sfx.call::<_, Value>(("txtScrl2.WAV",))?;
    let _ = stop_sound.call::<_, ()>(("keyboard.imu",));

    wait_for_message.call::<_, ()>((manny.clone(),))?;

    ignore_boxes.call::<_, ()>((manny.clone(), false))?;
    pop_costume.call::<_, ()>((manny.clone(),))?;
    set_pos.call::<_, ()>((manny.clone(), 0.5_f32, 1.975_f32, 0.0_f32))?;
    set_rot.call::<_, ()>((manny.clone(), 0.0_f32, 120.761002_f32, 0.0_f32))?;
    enable_head_control.call::<_, ()>((true,))?;

    context.log_event("hotspot.demo.fallback computer".to_string());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hotspot_options_parse_computer() {
        let options = HotspotOptions::parse("computer").expect("parse");
        assert_eq!(options.slug(), HotspotSlug::Computer);
    }

    #[test]
    fn hotspot_options_rejects_unknown() {
        assert!(HotspotOptions::parse("unknown").is_err());
    }
}
