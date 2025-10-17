use std::collections::VecDeque;

use grim_stream::StateUpdate;

const OVERLAY_WIDTH: u32 = 640;
const OVERLAY_HEIGHT: u32 = 480;
const BACKGROUND_COLOR: [u8; 4] = [32, 36, 44, 255];
const PATH_COLOR: [u8; 4] = [90, 142, 238, 180];
const MANNY_COLOR: [u8; 4] = [51, 242, 217, 240];
const HISTORY_CAPACITY: usize = 512;
const MIN_SPAN: f32 = 1.0;
const BOUNDS_MARGIN: f32 = 0.5;

pub struct EngineFrame<'a> {
    pub width: u32,
    pub height: u32,
    pub pixels: &'a [u8],
}

pub struct LiveSceneState {
    width: u32,
    height: u32,
    buffer: Vec<u8>,
    last_position: Option<[f32; 3]>,
    bounds: Option<PositionBounds>,
    history: VecDeque<[f32; 3]>,
}

#[derive(Clone, Copy)]
struct PositionBounds {
    min_x: f32,
    max_x: f32,
    min_z: f32,
    max_z: f32,
}

impl PositionBounds {
    fn new(x: f32, z: f32) -> Self {
        Self {
            min_x: x - BOUNDS_MARGIN,
            max_x: x + BOUNDS_MARGIN,
            min_z: z - BOUNDS_MARGIN,
            max_z: z + BOUNDS_MARGIN,
        }
    }

    fn include(&mut self, x: f32, z: f32) {
        self.min_x = self.min_x.min(x - BOUNDS_MARGIN);
        self.max_x = self.max_x.max(x + BOUNDS_MARGIN);
        self.min_z = self.min_z.min(z - BOUNDS_MARGIN);
        self.max_z = self.max_z.max(z + BOUNDS_MARGIN);
    }

    fn span_x(&self) -> f32 {
        (self.max_x - self.min_x).abs().max(MIN_SPAN)
    }

    fn span_z(&self) -> f32 {
        (self.max_z - self.min_z).abs().max(MIN_SPAN)
    }
}

impl LiveSceneState {
    pub fn new() -> Self {
        let width = OVERLAY_WIDTH;
        let height = OVERLAY_HEIGHT;
        Self {
            width,
            height,
            buffer: vec![0u8; (width * height * 4) as usize],
            last_position: None,
            bounds: None,
            history: VecDeque::with_capacity(HISTORY_CAPACITY),
        }
    }

    pub fn compose_frame(&mut self) -> Option<EngineFrame<'_>> {
        self.render_engine_overlay()
    }

    pub fn ingest_state_update<'a>(&'a mut self, update: &StateUpdate) -> Option<EngineFrame<'a>> {
        if let Some(position) = update.position {
            self.last_position = Some(position);
            match self.bounds.as_mut() {
                Some(bounds) => bounds.include(position[0], position[2]),
                None => self.bounds = Some(PositionBounds::new(position[0], position[2])),
            }
            self.push_history(position);
        }

        self.render_engine_overlay()
    }

    fn push_history(&mut self, position: [f32; 3]) {
        if let Some(last) = self.history.back() {
            let dx = last[0] - position[0];
            let dz = last[2] - position[2];
            if (dx * dx + dz * dz) < 0.0001 {
                return;
            }
        }
        if self.history.len() == HISTORY_CAPACITY {
            self.history.pop_front();
        }
        self.history.push_back(position);
    }

    fn render_engine_overlay(&mut self) -> Option<EngineFrame<'_>> {
        self.clear_buffer();

        if let Some(bounds) = self.bounds {
            self.draw_history(bounds);
        }

        if let Some(position) = self.last_position {
            self.draw_manny(position);
        }

        Some(EngineFrame {
            width: self.width,
            height: self.height,
            pixels: &self.buffer,
        })
    }

    fn clear_buffer(&mut self) {
        for chunk in self.buffer.chunks_mut(4) {
            chunk.copy_from_slice(&BACKGROUND_COLOR);
        }
    }

    fn draw_history(&mut self, bounds: PositionBounds) {
        for point in self.history.iter() {
            if let Some((px, py)) = self.project(bounds, *point) {
                stamp_point(
                    &mut self.buffer,
                    self.width,
                    self.height,
                    px,
                    py,
                    PATH_COLOR,
                    1,
                );
            }
        }
    }

    fn draw_manny(&mut self, position: [f32; 3]) {
        if let Some(bounds) = self.bounds {
            if let Some((px, py)) = self.project(bounds, position) {
                stamp_point(
                    &mut self.buffer,
                    self.width,
                    self.height,
                    px,
                    py,
                    MANNY_COLOR,
                    4,
                );
            }
        }
    }

    fn project(&self, bounds: PositionBounds, position: [f32; 3]) -> Option<(i32, i32)> {
        if self.width == 0 || self.height == 0 {
            return None;
        }
        let span_x = bounds.span_x();
        let span_z = bounds.span_z();
        let x_norm = ((position[0] - bounds.min_x) / span_x).clamp(0.0, 1.0);
        let z_norm = ((position[2] - bounds.min_z) / span_z).clamp(0.0, 1.0);

        let px = (x_norm * (self.width as f32 - 1.0)).round() as i32;
        let py = ((1.0 - z_norm) * (self.height as f32 - 1.0)).round() as i32;
        Some((px, py))
    }
}

fn stamp_point(
    buffer: &mut [u8],
    width: u32,
    height: u32,
    px: i32,
    py: i32,
    color: [u8; 4],
    radius: i32,
) {
    let width_i = width as i32;
    let height_i = height as i32;

    for y in (py - radius)..=(py + radius) {
        if y < 0 || y >= height_i {
            continue;
        }
        for x in (px - radius)..=(px + radius) {
            if x < 0 || x >= width_i {
                continue;
            }
            let offset = ((y as u32 * width) + x as u32) as usize * 4;
            buffer[offset..offset + 4].copy_from_slice(&color);
        }
    }
}
