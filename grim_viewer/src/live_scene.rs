use std::{
    collections::{BTreeMap, HashMap, HashSet},
    path::{Path, PathBuf},
    sync::Arc,
    time::Instant,
};

use anyhow::{Context, Result};
use glam::Vec3;
use grim_formats::SnmFile;
use grim_stream::{CoverageCounter, StateUpdate};

use crate::movie::{
    catalog::{MovieCatalog, MovieSource, normalize_movie_key},
    playback::{MovieAsset, MoviePlayback, Playback},
};

use crate::scene::{
    CameraProjector, EntityOrientation, MovementTrace, SceneEntityKind, ViewerScene,
    load_hotspot_event_log, load_lua_geometry_snapshot, load_movement_trace,
    load_scene_from_timeline, print_scene_summary,
};
use crate::texture::{decode_asset_texture, load_asset_bytes, load_zbm_seed};

#[derive(Debug, Clone)]
pub struct LiveSceneConfig {
    pub assets_manifest: PathBuf,
    pub timeline_manifest: PathBuf,
    pub geometry_snapshot: Option<PathBuf>,
    pub movement_log: Option<PathBuf>,
    pub hotspot_log: Option<PathBuf>,
    pub active_asset: Option<String>,
    pub movie_roots: Vec<PathBuf>,
}

impl LiveSceneConfig {
    pub fn from_args(args: &crate::Args) -> Result<Option<Self>> {
        let repo_root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..");
        println!(
            "[grim_viewer] resolving assets relative to {}",
            repo_root.display()
        );

        let assets_manifest = match args
            .scene_assets_manifest
            .clone()
            .or_else(|| locate(&repo_root, &["artifacts/manny_office_assets.json"]))
        {
            Some(path) => path,
            None => return Ok(None),
        };

        let timeline_manifest = match args.scene_timeline.clone().or_else(|| {
            locate(
                &repo_root,
                &[
                    "artifacts/run_cache/manny_office_timeline.json",
                    "tools/tests/manny_office_timeline.json",
                ],
            )
        }) {
            Some(path) => path,
            None => return Ok(None),
        };

        let geometry_snapshot = args
            .scene_geometry
            .clone()
            .or_else(|| locate(&repo_root, &["artifacts/run_cache/manny_geometry.json"]));

        let movement_log = args.scene_movement_log.clone().or_else(|| {
            locate(
                &repo_root,
                &[
                    "artifacts/run_cache/manny_movement_log.json",
                    "tools/tests/movement_log.json",
                ],
            )
        });

        let hotspot_log = args.scene_hotspot_log.clone().or_else(|| {
            locate(
                &repo_root,
                &[
                    "artifacts/run_cache/manny_hotspot_events.json",
                    "tools/tests/hotspot_events.json",
                ],
            )
        });

        let mut movie_roots = Vec::new();
        for relative in [
            "dev-install/MoviesHD",
            "dev-install/Movies",
            "dev-install/extracted",
            "extracted",
            "artifacts/extracted",
        ] {
            let path = repo_root.join(relative);
            if path.exists() {
                println!(
                    "[grim_viewer] movie root candidate present: {}",
                    path.display()
                );
                movie_roots.push(path);
            }
        }
        println!("[grim_viewer] movie roots collected: {}", movie_roots.len());

        let active_asset = args
            .scene_active_asset
            .clone()
            .or_else(|| Some(String::from("mo_0_ddtws.bm")));

        Ok(Some(Self {
            assets_manifest,
            timeline_manifest,
            geometry_snapshot,
            movement_log,
            hotspot_log,
            active_asset,
            movie_roots,
        }))
    }
}

struct ScenePlate {
    width: u32,
    height: u32,
    pixels: Vec<u8>,
    _asset_name: String,
}

#[derive(Default)]
struct EngineRuntimeState {
    seq: u64,
    host_time_ns: u64,
    frame: Option<u32>,
    last_position: Option<[f32; 3]>,
    last_yaw: Option<f32>,
    active_setup: Option<String>,
    active_hotspot: Option<String>,
    coverage: BTreeMap<String, u64>,
    active_movie: Option<String>,
    movie_started_at: Option<Instant>,
}

impl EngineRuntimeState {
    fn apply_coverage(&mut self, counters: &[CoverageCounter]) {
        for counter in counters {
            self.coverage.insert(counter.key.clone(), counter.value);
        }
    }
}

pub struct EngineFrame<'a> {
    pub width: u32,
    pub height: u32,
    pub pixels: &'a [u8],
}

const COLOR_BACKGROUND_FALLBACK: [u8; 4] = [32, 36, 44, 255];
const COLOR_MANNY_FILL: [u8; 4] = [51, 242, 217, 240];
const COLOR_MANNY_OUTLINE: [u8; 4] = [18, 128, 112, 200];
const COLOR_ORIENTATION: [u8; 4] = [250, 238, 120, 220];
const COLOR_PATH: [u8; 4] = [190, 160, 255, 140];
const COLOR_ENTITY_ACTOR: [u8; 4] = [64, 208, 150, 200];
const COLOR_ENTITY_OBJECT: [u8; 4] = [64, 153, 242, 190];
const COLOR_ENTITY_INTEREST: [u8; 4] = [217, 179, 64, 200];
const MANNY_ARROW_LENGTH: f32 = 0.35;

pub struct LiveSceneState {
    scene: ViewerScene,
    _movement_trace: Option<MovementTrace>,
    plate: Option<ScenePlate>,
    overlay_buffer: Vec<u8>,
    runtime: EngineRuntimeState,
    manny_index: Option<usize>,
    movie_roots: Vec<PathBuf>,
    movie_index: MovieCatalog,
    movie_assets: HashMap<String, Arc<SnmFile>>,
    movie_playback: Option<Playback>,
    movie_frame_rgba: Vec<u8>,
    missing_movies: HashSet<String>,
}

impl LiveSceneState {
    pub fn load(config: LiveSceneConfig) -> Result<Self> {
        let geometry = match config.geometry_snapshot.as_ref() {
            Some(path) => match load_lua_geometry_snapshot(path) {
                Ok(snapshot) => Some(snapshot),
                Err(err) => {
                    eprintln!(
                        "[grim_viewer] warning: failed to load geometry snapshot {}: {err:?}",
                        path.display()
                    );
                    None
                }
            },
            None => None,
        };

        let mut scene = match load_scene_from_timeline(
            &config.timeline_manifest,
            &config.assets_manifest,
            config.active_asset.as_deref(),
            geometry.as_ref(),
        ) {
            Ok(scene) => scene,
            Err(err) => {
                eprintln!(
                    "[grim_viewer] warning: failed to load Manny timeline {}: {err:?}; falling back to empty scene",
                    config.timeline_manifest.display()
                );
                ViewerScene {
                    entities: Vec::new(),
                    position_bounds: None,
                    timeline: None,
                    movement: None,
                    hotspot_events: Vec::new(),
                    camera: None,
                    active_setup: None,
                }
            }
        };

        let mut movement_trace: Option<MovementTrace> = None;
        if let Some(path) = config.movement_log.as_ref() {
            match load_movement_trace(path) {
                Ok(trace) => {
                    movement_trace = Some(trace.clone());
                    scene.attach_movement_trace(trace);
                }
                Err(err) => {
                    eprintln!(
                        "[grim_viewer] warning: failed to attach movement trace from {}: {err:?}",
                        path.display()
                    );
                }
            }
        }

        if let Some(path) = config.hotspot_log.as_ref() {
            match load_hotspot_event_log(path) {
                Ok(events) => scene.attach_hotspot_events(events),
                Err(err) => eprintln!(
                    "[grim_viewer] warning: failed to attach hotspot events from {}: {err:?}",
                    path.display()
                ),
            }
        }

        print_scene_summary(&scene);

        let manny_index = scene
            .entities
            .iter()
            .position(|entity| entity.name.eq_ignore_ascii_case("manny"));

        let mut runtime = EngineRuntimeState::default();
        runtime.active_setup = scene.active_setup.clone();
        if let Some(index) = manny_index {
            if let Some(position) = scene.entities.get(index).and_then(|entity| entity.position) {
                runtime.last_position = Some(position);
            }
            if let Some(rotation) = scene.entities.get(index).and_then(|entity| entity.rotation) {
                runtime.last_yaw = Some(rotation[1]);
            }
        }

        let plate = load_scene_plate(&config.assets_manifest, config.active_asset.as_deref());

        let overlay_buffer = plate
            .as_ref()
            .map(|plate| plate.pixels.clone())
            .unwrap_or_else(|| COLOR_BACKGROUND_FALLBACK.to_vec());

        let movie_roots = config.movie_roots.clone();
        let movie_root_label = if movie_roots.is_empty() {
            "<unspecified>".to_string()
        } else {
            movie_roots
                .iter()
                .map(|path| path.display().to_string())
                .collect::<Vec<_>>()
                .join(", ")
        };
        if !movie_roots.is_empty() {
            println!("[grim_viewer] scanning movie roots: {}", movie_root_label);
        }
        let movie_index = if movie_roots.is_empty() {
            MovieCatalog::default()
        } else {
            match MovieCatalog::from_roots(&movie_roots) {
                Ok(catalog) => {
                    let stats = catalog.stats();
                    if stats.total == 0 {
                        eprintln!(
                            "[grim_viewer] warning: no fullscreen movies found under {}",
                            movie_root_label
                        );
                    } else {
                        println!(
                            "[grim_viewer] indexed {} movie(s) from {} (SNM: {}, OGV fallback: {})",
                            stats.total, movie_root_label, stats.snm, stats.ogv
                        );
                    }
                    catalog
                }
                Err(err) => {
                    eprintln!(
                        "[grim_viewer] warning: failed to index movies under {}: {err:?}",
                        movie_root_label
                    );
                    MovieCatalog::default()
                }
            }
        };

        Ok(Self {
            scene,
            _movement_trace: movement_trace,
            plate,
            overlay_buffer,
            runtime,
            manny_index,
            movie_roots,
            movie_index,
            movie_assets: HashMap::new(),
            movie_playback: None,
            movie_frame_rgba: Vec::new(),
            missing_movies: HashSet::new(),
        })
    }

    #[allow(dead_code)]
    pub fn scene(&self) -> &ViewerScene {
        &self.scene
    }

    #[allow(dead_code)]
    pub fn scene_mut(&mut self) -> &mut ViewerScene {
        &mut self.scene
    }

    pub fn compose_engine_frame(&mut self) -> Option<EngineFrame<'_>> {
        self.render_engine_overlay()
    }

    pub fn ingest_state_update<'a>(&'a mut self, update: &StateUpdate) -> Option<EngineFrame<'a>> {
        self.runtime.seq = update.seq;
        self.runtime.host_time_ns = update.host_time_ns;
        self.runtime.frame = update.frame;

        if let Some(position) = update.position {
            self.runtime.last_position = Some(position);
            if let Some(index) = self.manny_index {
                if let Some(entity) = self.scene.entities.get_mut(index) {
                    entity.position = Some(position);
                }
            }
            if let Some(bounds) = self.scene.position_bounds.as_mut() {
                bounds.update(position);
            }
        }

        if let Some(yaw) = update.yaw {
            self.runtime.last_yaw = Some(yaw);
            if let Some(index) = self.manny_index {
                if let Some(entity) = self.scene.entities.get_mut(index) {
                    entity.rotation = Some([0.0, yaw, 0.0]);
                    entity.orientation = Some(EntityOrientation::from_degrees([0.0, yaw, 0.0]));
                }
            }
        }

        if let Some(setup) = update.active_setup.as_ref() {
            self.runtime.active_setup = Some(setup.clone());
            self.scene.active_setup = Some(setup.clone());
        }

        if let Some(hotspot) = update.active_hotspot.as_ref() {
            self.runtime.active_hotspot = Some(hotspot.clone());
        }

        for event in &update.events {
            if let Some(movie) = event.strip_prefix("cut_scene.fullscreen.start ") {
                self.start_fullscreen_movie(movie, update.host_time_ns);
            } else if event.starts_with("cut_scene.fullscreen.end ") {
                self.stop_fullscreen_movie();
            }
        }

        self.runtime.apply_coverage(&update.coverage);
        self.render_engine_overlay()
    }

    fn start_fullscreen_movie(&mut self, movie: &str, host_time_ns: u64) {
        println!(
            "[grim_viewer] start_fullscreen_movie requested: {} at host_time_ns {}",
            movie, host_time_ns
        );
        self.runtime.active_movie = Some(movie.to_string());
        self.runtime.movie_started_at = Some(Instant::now());
        self.movie_playback = self.prepare_movie_playback(movie, host_time_ns);
    }

    fn stop_fullscreen_movie(&mut self) {
        self.runtime.active_movie = None;
        self.runtime.movie_started_at = None;
        self.movie_playback = None;
    }

    fn prepare_movie_playback(&mut self, movie: &str, host_time_ns: u64) -> Option<Playback> {
        let key = normalize_movie_key(movie);
        match self.load_movie_asset(&key) {
            Ok(Some(asset)) => {
                self.missing_movies.remove(&key);
                if let MovieAsset::Ogv(ref path) = asset {
                    println!(
                        "[grim_viewer] using OGV fallback for '{}' ({})",
                        movie,
                        path.display()
                    );
                }
                let backend = match &asset {
                    MovieAsset::Snm(_) => "snm",
                    MovieAsset::Ogv(_) => "theora",
                };
                match Playback::new(movie.to_string(), asset, host_time_ns) {
                    Ok(playback) => Some(playback),
                    Err(err) => {
                        if self.missing_movies.insert(key.clone()) {
                            eprintln!(
                                "[grim_viewer] warning: failed to initialize movie '{}' (backend={}): {err:?}",
                                movie, backend
                            );
                        }
                        None
                    }
                }
            }
            Ok(None) => {
                if self.missing_movies.insert(key.clone()) {
                    let root = if self.movie_roots.is_empty() {
                        "<unspecified>".to_string()
                    } else {
                        self.movie_roots
                            .iter()
                            .map(|path| path.display().to_string())
                            .collect::<Vec<_>>()
                            .join(", ")
                    };
                    eprintln!(
                        "[grim_viewer] warning: cutscene movie '{}' not found under {}",
                        movie, root
                    );
                }
                None
            }
            Err(err) => {
                if self.missing_movies.insert(key.clone()) {
                    eprintln!(
                        "[grim_viewer] warning: failed to load movie '{}': {err:?}",
                        movie
                    );
                }
                None
            }
        }
    }

    fn load_movie_asset(&mut self, key: &str) -> Result<Option<MovieAsset>> {
        if let Some(cached) = self.movie_assets.get(key) {
            return Ok(Some(MovieAsset::Snm(cached.clone())));
        }
        let Some(source) = self.movie_index.get(key) else {
            return Ok(None);
        };
        match source {
            MovieSource::Snm(path) => {
                let snm = Arc::new(
                    SnmFile::open(path)
                        .with_context(|| format!("failed to open SNM {}", path.display()))?,
                );
                self.movie_assets.insert(key.to_string(), snm.clone());
                Ok(Some(MovieAsset::Snm(snm)))
            }
            MovieSource::Ogv(path) => Ok(Some(MovieAsset::Ogv(path.clone()))),
        }
    }

    fn render_engine_overlay(&mut self) -> Option<EngineFrame<'_>> {
        let (width, height) = if let Some(plate) = self.plate.as_ref() {
            if self.overlay_buffer.len() != plate.pixels.len() {
                self.overlay_buffer = plate.pixels.clone();
            } else {
                self.overlay_buffer.copy_from_slice(&plate.pixels);
            }
            (plate.width, plate.height)
        } else {
            if self.overlay_buffer.len() != COLOR_BACKGROUND_FALLBACK.len() {
                self.overlay_buffer = COLOR_BACKGROUND_FALLBACK.to_vec();
            } else {
                self.overlay_buffer
                    .copy_from_slice(&COLOR_BACKGROUND_FALLBACK);
            }
            return Some(EngineFrame {
                width: 1,
                height: 1,
                pixels: &self.overlay_buffer,
            });
        };

        if width == 0 || height == 0 {
            return None;
        }

        let mut drop_playback = false;
        if let Some(playback) = self.movie_playback.as_mut() {
            let movie_width = playback.width();
            let movie_height = playback.height();
            let movie_name = playback.name().to_string();
            match playback.frame_for_host_time(self.runtime.host_time_ns) {
                Ok(pixels) => {
                    let expected_len = (movie_width as usize)
                        .saturating_mul(movie_height as usize)
                        .saturating_mul(4);
                    if pixels.len() != expected_len {
                        let actual_len = pixels.len();
                        let status = playback.status();
                        eprintln!(
                            "[grim_viewer] warning: movie '{}' produced {} bytes (expected {}) (backend={}, frame={:?}, eos={})",
                            movie_name,
                            actual_len,
                            expected_len,
                            status.backend,
                            status.current_frame,
                            status.end_of_stream
                        );
                        drop_playback = true;
                    } else {
                        if self.movie_frame_rgba.len() != expected_len {
                            self.movie_frame_rgba.resize(expected_len, 0);
                        }
                        self.movie_frame_rgba[..expected_len].copy_from_slice(pixels);
                        return Some(EngineFrame {
                            width: movie_width,
                            height: movie_height,
                            pixels: &self.movie_frame_rgba,
                        });
                    }
                }
                Err(err) => {
                    let status = playback.status();
                    eprintln!(
                        "[grim_viewer] warning: movie playback for '{}' failed: {err:?} (backend={}, frame={:?}, eos={})",
                        movie_name, status.backend, status.current_frame, status.end_of_stream
                    );
                    drop_playback = true;
                }
            }
        }
        if drop_playback {
            self.movie_playback = None;
        }

        if self.runtime.active_movie.is_some() {
            for pixel in self.overlay_buffer.chunks_mut(4) {
                pixel.copy_from_slice(&[0, 0, 0, 255]);
            }
            return Some(EngineFrame {
                width,
                height,
                pixels: &self.overlay_buffer,
            });
        }

        if let Some(projector) = self.scene.camera_projector(width as f32 / height as f32) {
            draw_movement_trace(
                &mut self.overlay_buffer,
                width,
                height,
                &projector,
                self.scene.movement_trace(),
            );
            draw_scene_entities(
                &mut self.overlay_buffer,
                width,
                height,
                &projector,
                &self.scene,
                self.manny_index,
            );
        }

        Some(EngineFrame {
            width,
            height,
            pixels: &self.overlay_buffer,
        })
    }
}

fn load_scene_plate(manifest_path: &Path, asset: Option<&str>) -> Option<ScenePlate> {
    let asset = asset?;
    match load_asset_bytes(manifest_path, asset) {
        Ok((resolved_name, asset_bytes, _archive_path)) => {
            let seed_bitmap = match load_zbm_seed(manifest_path, &resolved_name) {
                Ok(seed) => seed,
                Err(err) => {
                    eprintln!(
                        "[grim_viewer] warning: failed to resolve base bitmap for {}: {err}",
                        resolved_name
                    );
                    None
                }
            };

            match decode_asset_texture(&resolved_name, &asset_bytes, seed_bitmap.as_ref()) {
                Ok(preview) => {
                    let width = preview.width.max(1);
                    let height = preview.height.max(1);
                    Some(ScenePlate {
                        width,
                        height,
                        pixels: preview.data,
                        _asset_name: resolved_name,
                    })
                }
                Err(err) => {
                    eprintln!(
                        "[grim_viewer] warning: failed to decode asset {}: {err}",
                        resolved_name
                    );
                    None
                }
            }
        }
        Err(err) => {
            eprintln!(
                "[grim_viewer] warning: failed to load asset '{}' from {}: {err}",
                asset,
                manifest_path.display()
            );
            None
        }
    }
}

fn draw_movement_trace(
    buffer: &mut [u8],
    width: u32,
    height: u32,
    projector: &CameraProjector,
    trace: Option<&MovementTrace>,
) {
    let Some(trace) = trace else {
        return;
    };

    let mut previous: Option<(i32, i32)> = None;
    for sample in &trace.samples {
        let Some((x, y)) = project_point(projector, sample.position, width, height) else {
            continue;
        };
        if let Some((px, py)) = previous {
            draw_line(buffer, width, height, px, py, x, y, COLOR_PATH, 1);
        }
        previous = Some((x, y));
    }
}

fn draw_scene_entities(
    buffer: &mut [u8],
    width: u32,
    height: u32,
    projector: &CameraProjector,
    scene: &ViewerScene,
    manny_index: Option<usize>,
) {
    if let Some(index) = manny_index {
        if let Some(manny) = scene.entities.get(index) {
            if let Some(position) = manny.position {
                if let Some((mx, my)) = project_point(projector, position, width, height) {
                    let radius = scaled_radius(width, height, 0.018, 4.0);
                    draw_circle(
                        buffer,
                        width,
                        height,
                        mx,
                        my,
                        radius + 2,
                        COLOR_MANNY_OUTLINE,
                    );
                    draw_circle(buffer, width, height, mx, my, radius, COLOR_MANNY_FILL);

                    if let Some(orientation) = manny.orientation {
                        let forward = Vec3::from_array(orientation.forward);
                        let origin = Vec3::from_array(position);
                        let tip = origin + forward * MANNY_ARROW_LENGTH;
                        if let Some((tx, ty)) =
                            project_point(projector, tip.to_array(), width, height)
                        {
                            let thickness = scaled_radius(width, height, 0.006, 1.0);
                            draw_line(
                                buffer,
                                width,
                                height,
                                mx,
                                my,
                                tx,
                                ty,
                                COLOR_ORIENTATION,
                                thickness,
                            );
                        }
                    }
                }
            }
        }
    }

    for (index, entity) in scene.entities.iter().enumerate() {
        if Some(index) == manny_index {
            continue;
        }
        let Some(position) = entity.position else {
            continue;
        };
        let Some((px, py)) = project_point(projector, position, width, height) else {
            continue;
        };

        let color = entity_color(entity.kind);
        let radius = entity_radius(entity.kind, width, height);
        draw_circle(buffer, width, height, px, py, radius, color);
    }
}

fn entity_color(kind: SceneEntityKind) -> [u8; 4] {
    match kind {
        SceneEntityKind::Actor => COLOR_ENTITY_ACTOR,
        SceneEntityKind::Object => COLOR_ENTITY_OBJECT,
        SceneEntityKind::InterestActor => COLOR_ENTITY_INTEREST,
    }
}

fn entity_radius(kind: SceneEntityKind, width: u32, height: u32) -> i32 {
    match kind {
        SceneEntityKind::Actor => scaled_radius(width, height, 0.012, 3.0),
        SceneEntityKind::Object => scaled_radius(width, height, 0.010, 3.0),
        SceneEntityKind::InterestActor => scaled_radius(width, height, 0.009, 2.0),
    }
}

fn project_point(
    projector: &CameraProjector,
    position: [f32; 3],
    width: u32,
    height: u32,
) -> Option<(i32, i32)> {
    let ndc = projector.project(position)?;
    if !ndc[0].is_finite() || !ndc[1].is_finite() {
        return None;
    }

    let width_f = width.max(1) as f32;
    let height_f = height.max(1) as f32;

    let norm_x = ((ndc[0] + 1.0) * 0.5).clamp(0.0, 1.0);
    let norm_y = ((1.0 - ndc[1]) * 0.5).clamp(0.0, 1.0);

    let pixel_x = norm_x * (width_f - 1.0);
    let pixel_y = norm_y * (height_f - 1.0);

    Some((pixel_x.round() as i32, pixel_y.round() as i32))
}

fn draw_circle(
    buffer: &mut [u8],
    width: u32,
    height: u32,
    cx: i32,
    cy: i32,
    radius: i32,
    color: [u8; 4],
) {
    let radius = radius.max(0);
    if radius == 0 {
        blend_pixel(buffer, width, height, cx, cy, color);
        return;
    }
    let radius_sq = radius * radius;
    for dy in -radius..=radius {
        for dx in -radius..=radius {
            if dx * dx + dy * dy <= radius_sq {
                blend_pixel(buffer, width, height, cx + dx, cy + dy, color);
            }
        }
    }
}

fn draw_line(
    buffer: &mut [u8],
    width: u32,
    height: u32,
    mut x0: i32,
    mut y0: i32,
    x1: i32,
    y1: i32,
    color: [u8; 4],
    thickness: i32,
) {
    let dx = (x1 - x0).abs();
    let sx = if x0 < x1 { 1 } else { -1 };
    let dy = -(y1 - y0).abs();
    let sy = if y0 < y1 { 1 } else { -1 };
    let mut err = dx + dy;
    let radius = thickness.max(1);

    loop {
        draw_circle(buffer, width, height, x0, y0, radius, color);
        if x0 == x1 && y0 == y1 {
            break;
        }
        let e2 = err * 2;
        if e2 >= dy {
            err += dy;
            x0 += sx;
        }
        if e2 <= dx {
            err += dx;
            y0 += sy;
        }
    }
}

fn blend_pixel(buffer: &mut [u8], width: u32, height: u32, x: i32, y: i32, color: [u8; 4]) {
    if x < 0 || y < 0 {
        return;
    }
    if x >= width as i32 || y >= height as i32 {
        return;
    }

    let idx = ((y as u32 * width + x as u32) * 4) as usize;
    if idx + 4 > buffer.len() {
        return;
    }

    let alpha = color[3] as f32 / 255.0;
    if alpha <= 0.0 {
        return;
    }
    let inv_alpha = 1.0 - alpha;

    let pixel = &mut buffer[idx..idx + 4];
    for channel in 0..3 {
        let blended = pixel[channel] as f32 * inv_alpha + color[channel] as f32 * alpha;
        pixel[channel] = blended.clamp(0.0, 255.0) as u8;
    }
    pixel[3] = 255;
}

fn scaled_radius(width: u32, height: u32, fraction: f32, minimum: f32) -> i32 {
    let max_side = width.max(height) as f32;
    let scaled = (max_side * fraction).max(minimum);
    scaled.round().max(1.0) as i32
}

fn locate(repo_root: &Path, candidates: &[&str]) -> Option<PathBuf> {
    for relative in candidates {
        let path = repo_root.join(relative);
        if path.exists() {
            return Some(path);
        }
    }
    None
}
