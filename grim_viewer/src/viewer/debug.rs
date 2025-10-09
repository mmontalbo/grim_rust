use std::sync::OnceLock;

#[derive(Debug)]
pub struct DebugConfig {
    marker_size_override: Option<f32>,
    marker_size_scale: Option<f32>,
    marker_highlight_override: Option<f32>,
    marker_color_override: Option<[f32; 3]>,
    log_marker_projection: bool,
}

impl DebugConfig {
    fn from_env() -> Self {
        let marker_size_override = std::env::var("GRIM_VIEWER_MARKER_SIZE")
            .ok()
            .and_then(|v| v.parse().ok());
        let marker_size_scale = std::env::var("GRIM_VIEWER_MARKER_SCALE")
            .ok()
            .and_then(|v| v.parse().ok());
        let marker_highlight_override = std::env::var("GRIM_VIEWER_MARKER_HIGHLIGHT")
            .ok()
            .and_then(|v| v.parse().ok());
        let marker_color_override = std::env::var("GRIM_VIEWER_MARKER_COLOR")
            .ok()
            .and_then(|v| parse_color(&v));
        let log_marker_projection = std::env::var("GRIM_VIEWER_LOG_MARKERS")
            .map(|v| matches!(v.as_str(), "1" | "true" | "on" | "yes"))
            .unwrap_or(false);

        Self {
            marker_size_override,
            marker_size_scale,
            marker_highlight_override,
            marker_color_override,
            log_marker_projection,
        }
    }

    pub fn apply_marker_overrides(
        &self,
        label: Option<&str>,
        position: [f32; 3],
        ndc: [f32; 2],
        size: &mut f32,
        color: &mut [f32; 3],
        highlight: &mut f32,
    ) {
        if let Some(value) = self.marker_size_override {
            *size = value;
        }
        if let Some(scale) = self.marker_size_scale {
            *size *= scale;
        }
        if let Some(value) = self.marker_highlight_override {
            *highlight = value;
        }
        if let Some(override_color) = self.marker_color_override {
            *color = override_color;
        }

        if self.log_marker_projection {
            if let Some(label) = label {
                eprintln!(
                    "[grim_viewer][marker] {label}: world=({:.3}, {:.3}, {:.3}) ndc=({:.3}, {:.3}) size={:.3} highlight={:.3}",
                    position[0], position[1], position[2], ndc[0], ndc[1], *size, *highlight
                );
            } else {
                eprintln!(
                    "[grim_viewer][marker] world=({:.3}, {:.3}, {:.3}) ndc=({:.3}, {:.3}) size={:.3} highlight={:.3}",
                    position[0], position[1], position[2], ndc[0], ndc[1], *size, *highlight
                );
            }
        }
    }
}

fn parse_color(value: &str) -> Option<[f32; 3]> {
    let mut parts = value.split(',');
    let r = parts.next()?.trim().parse().ok()?;
    let g = parts.next()?.trim().parse().ok()?;
    let b = parts.next()?.trim().parse().ok()?;
    Some([r, g, b])
}

pub fn config() -> &'static DebugConfig {
    static CONFIG: OnceLock<DebugConfig> = OnceLock::new();
    CONFIG.get_or_init(DebugConfig::from_env)
}
