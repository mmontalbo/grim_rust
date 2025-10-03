use std::{
    borrow::Cow,
    collections::{BTreeMap, BTreeSet},
    fs::File,
    io::{Read, Seek, SeekFrom},
    path::{Path, PathBuf},
    sync::{Arc, mpsc},
};

use anyhow::{Context, Result, anyhow, bail, ensure};
use bytemuck::{Pod, Zeroable, cast_slice};
use clap::Parser;
use grim_formats::decode_bm;
use image::{ColorType, ImageEncoder, codecs::png::PngEncoder};
use pollster::FutureExt;
#[cfg(feature = "audio")]
use rodio::OutputStream;
use serde::Deserialize;
use serde_json::Value;
use wgpu::{
    Backends, COPY_BYTES_PER_ROW_ALIGNMENT, InstanceDescriptor, InstanceFlags, Maintain,
    SurfaceError, util::DeviceExt,
};
use winit::{
    dpi::PhysicalSize,
    event::{ElementState, Event, KeyEvent, WindowEvent},
    event_loop::{ControlFlow, EventLoop},
    keyboard::{Key, NamedKey},
    window::{Window, WindowBuilder},
};

#[derive(Parser, Debug)]
#[command(about = "Minimal viewer stub that boots wgpu and rodio", version)]
struct Args {
    /// Asset manifest JSON produced by grim_engine --asset-manifest
    #[arg(long, default_value = "artifacts/manny_office_assets.json")]
    manifest: PathBuf,

    /// Asset to load from the LAB archives for inspection
    #[arg(long, default_value = "mo_tube_balloon.zbm")]
    asset: String,

    /// Optional boot timeline manifest produced by grim_engine --timeline-json
    #[arg(long)]
    timeline: Option<PathBuf>,

    /// When set, write the decoded bitmap to disk (PNG) before launching the viewer
    #[arg(long)]
    dump_frame: Option<PathBuf>,

    /// When set, render the textured quad offscreen and dump the final output PNG
    #[arg(long)]
    dump_render: Option<PathBuf>,

    /// Run the offscreen renderer and validate the output against the decoded bitmap
    #[arg(long)]
    verify_render: bool,

    /// Maximum allowed fraction (0-1) of pixels that may diverge in the render diff
    #[arg(long, default_value_t = 0.01)]
    render_diff_threshold: f32,

    /// Skip creating a winit window/event loop; useful for headless automation
    #[arg(long)]
    headless: bool,
}

fn main() -> Result<()> {
    let args = Args::parse();

    env_logger::init();

    ensure!(
        (0.0..=1.0).contains(&args.render_diff_threshold),
        "render_diff_threshold must be between 0 and 1 (got {})",
        args.render_diff_threshold
    );

    let (asset_name, asset_bytes, source_archive) =
        load_asset_bytes(&args.manifest, &args.asset).context("loading requested asset")?;
    println!(
        "Loaded {} ({} bytes) from {} (manifest: {})",
        asset_name,
        asset_bytes.len(),
        source_archive.display(),
        args.manifest.display()
    );

    let decode_result = decode_asset_texture(&asset_name, &asset_bytes);

    if let Some(output_path) = args.dump_frame.as_ref() {
        let preview = decode_result
            .as_ref()
            .map_err(|err| anyhow!("decoding bitmap for --dump-frame: {err}"))?;
        let stats = dump_texture_to_png(preview, output_path)
            .with_context(|| format!("writing PNG to {}", output_path.display()))?;
        println!(
            "Bitmap frame exported to {} ({}x{} codec {} frame count {})",
            output_path.display(),
            preview.width,
            preview.height,
            preview.codec,
            preview.frame_count
        );
        println!(
            "  luminance avg {:.2}, min {}, max {}, opaque pixels {} / {}",
            stats.mean_luma,
            stats.min_luma,
            stats.max_luma,
            stats.opaque_pixels,
            stats.total_pixels
        );
        println!(
            "  quadrant luma means (TL, TR, BL, BR): {:.2}, {:.2}, {:.2}, {:.2}",
            stats.quadrant_means[0],
            stats.quadrant_means[1],
            stats.quadrant_means[2],
            stats.quadrant_means[3]
        );
    }

    if args.verify_render || args.dump_render.is_some() {
        let preview = decode_result
            .as_ref()
            .map_err(|err| anyhow!("decoding bitmap for render verification: {err}"))?;
        let destination = args.dump_render.as_deref();
        let verification = render_texture_offscreen(preview, destination)
            .context("running offscreen render verification")?;
        let stats = &verification.stats;
        if let Some(path) = destination {
            println!(
                "Rendered quad exported to {} ({}x{} post-raster)",
                path.display(),
                preview.width,
                preview.height
            );
        } else {
            println!(
                "Rendered quad verification completed ({}x{} offscreen)",
                preview.width, preview.height
            );
        }
        println!(
            "  render luma avg {:.2}, min {}, max {}, opaque pixels {} / {}",
            stats.mean_luma,
            stats.min_luma,
            stats.max_luma,
            stats.opaque_pixels,
            stats.total_pixels
        );
        println!(
            "  render quadrant luma means (TL, TR, BL, BR): {:.2}, {:.2}, {:.2}, {:.2}",
            stats.quadrant_means[0],
            stats.quadrant_means[1],
            stats.quadrant_means[2],
            stats.quadrant_means[3]
        );
        let diff = &verification.diff;
        let mismatch_ratio = diff_mismatch_ratio(diff);
        let mismatch_pct = mismatch_ratio * 100.0;
        println!(
            "  render diff: mismatched_pixels={} ({:.4}%), max_abs_diff={}, mean_abs_diff={:.3}",
            diff.mismatched_pixels, mismatch_pct, diff.max_abs_diff, diff.mean_abs_diff
        );
        println!(
            "  render diff quadrant mismatch ratios (TL, TR, BL, BR): {:.4}, {:.4}, {:.4}, {:.4}",
            diff.quadrant_mismatch[0],
            diff.quadrant_mismatch[1],
            diff.quadrant_mismatch[2],
            diff.quadrant_mismatch[3]
        );
        validate_render_diff(diff, args.render_diff_threshold)?;
    }

    let scene_data = match args.timeline.as_ref() {
        Some(path) => {
            let scene = load_scene_from_timeline(path)
                .with_context(|| format!("loading timeline manifest {}", path.display()))?;
            Some(scene)
        }
        None => None,
    };

    if let Some(scene) = scene_data.as_ref() {
        println!();
        println!(
            "Scene bootstrap: {} entit{} from timeline manifest",
            scene.entities.len(),
            if scene.entities.len() == 1 {
                "y"
            } else {
                "ies"
            }
        );
        for entity in &scene.entities {
            println!("  - {}", entity.describe());
        }
        if !scene.entities.is_empty() {
            println!("\nUse ←/→ to cycle entity focus while the viewer is running.");
            println!(
                "Markers overlay: green/blue squares mark entities; red highlights the current selection."
            );
        }
        println!();
    }

    if args.headless {
        // Propagate any decoding failure before exiting early.
        decode_result?;
        println!("Headless mode requested; viewer window bootstrap skipped.");
        return Ok(());
    }

    let scene = scene_data.map(Arc::new);

    // Bring up the audio stack so the renderer can acquire an output stream later.
    init_audio()?;

    let event_loop = EventLoop::new().context("creating winit event loop")?;
    let window = Arc::new(
        WindowBuilder::new()
            .with_title(format!("Grim Viewer - {}", asset_name))
            .with_inner_size(PhysicalSize::new(1280, 720))
            .build(&event_loop)
            .context("creating viewer window")?,
    );

    let mut state = ViewerState::new(
        window,
        &asset_name,
        asset_bytes,
        decode_result,
        scene.clone(),
    )
    .block_on()?;

    event_loop
        .run(move |event, target| {
            target.set_control_flow(ControlFlow::Poll);

            match event {
                Event::WindowEvent { window_id, event } if window_id == state.window().id() => {
                    match event {
                        WindowEvent::CloseRequested => target.exit(),
                        WindowEvent::KeyboardInput {
                            event:
                                KeyEvent {
                                    logical_key: Key::Named(NamedKey::Escape),
                                    state: ElementState::Pressed,
                                    ..
                                },
                            ..
                        } => target.exit(),
                        WindowEvent::KeyboardInput {
                            event:
                                KeyEvent {
                                    logical_key: Key::Named(NamedKey::ArrowRight),
                                    state: ElementState::Pressed,
                                    ..
                                },
                            ..
                        } => state.next_entity(),
                        WindowEvent::KeyboardInput {
                            event:
                                KeyEvent {
                                    logical_key: Key::Named(NamedKey::ArrowLeft),
                                    state: ElementState::Pressed,
                                    ..
                                },
                            ..
                        } => state.previous_entity(),
                        WindowEvent::Resized(new_size) => state.resize(new_size),
                        WindowEvent::RedrawRequested => match state.render() {
                            Ok(_) => {}
                            Err(SurfaceError::Lost) => state.resize(state.size()),
                            Err(SurfaceError::OutOfMemory) => target.exit(),
                            Err(err) => eprintln!("[grim_viewer] render error: {err:?}"),
                        },
                        _ => {}
                    }
                }
                Event::AboutToWait => state.window().request_redraw(),
                _ => {}
            }
        })
        .context("running viewer application")?;
    Ok(())
}

#[derive(Debug, Deserialize)]
struct AssetManifest {
    found: Vec<AssetManifestEntry>,
}

#[derive(Debug, Deserialize)]
struct AssetManifestEntry {
    asset_name: String,
    archive_path: PathBuf,
    offset: u64,
    size: u32,
    #[serde(default)]
    metadata: Option<AssetMetadataSummary>,
}

#[allow(dead_code)]
#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum AssetMetadataSummary {
    Bitmap {
        codec: u32,
        bits_per_pixel: u32,
        frames: u32,
        width: u32,
        height: u32,
        supported: bool,
    },
}

#[derive(Debug)]
struct ViewerScene {
    entities: Vec<SceneEntity>,
    position_bounds: Option<SceneBounds>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum SceneEntityKind {
    Actor,
    Object,
    InterestActor,
}

impl SceneEntityKind {
    fn label(self) -> &'static str {
        match self {
            SceneEntityKind::Actor => "Actor",
            SceneEntityKind::Object => "Object",
            SceneEntityKind::InterestActor => "Interest Actor",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct SceneEntityKey {
    kind: SceneEntityKind,
    name: String,
}

impl SceneEntityKey {
    fn new(kind: SceneEntityKind, name: String) -> Self {
        Self { kind, name }
    }
}

#[derive(Debug)]
struct SceneEntityBuilder {
    key: SceneEntityKey,
    created_by: Option<String>,
    methods: BTreeSet<String>,
    position: Option<[f32; 3]>,
    rotation: Option<[f32; 3]>,
    facing_target: Option<String>,
    last_played: Option<String>,
    last_looping: Option<String>,
    last_completed: Option<String>,
}

impl SceneEntityBuilder {
    fn new(kind: SceneEntityKind, name: String) -> Self {
        Self {
            key: SceneEntityKey::new(kind, name),
            created_by: None,
            methods: BTreeSet::new(),
            position: None,
            rotation: None,
            facing_target: None,
            last_played: None,
            last_looping: None,
            last_completed: None,
        }
    }

    fn apply_actor_snapshot(&mut self, value: &Value) {
        if self.created_by.is_none() {
            self.created_by = value.get("created_by").and_then(format_hook_reference);
        }

        if let Some(methods) = value
            .get("method_totals")
            .and_then(|totals| totals.as_object())
        {
            for key in methods.keys() {
                self.methods.insert(key.clone());
            }
        }

        if let Some(transform) = value.get("transform") {
            if let Some(position) = transform.get("position") {
                self.position = parse_vec3_object(position);
            }
            if let Some(rotation) = transform.get("rotation") {
                self.rotation = parse_vec3_object(rotation);
            }
            if let Some(facing) = transform
                .get("facing_target")
                .and_then(|v| v.as_str())
                .map(str::to_string)
            {
                self.facing_target = Some(facing);
            }
        }

        if let Some(chore) = value.get("chore_state") {
            if let Some(name) = chore
                .get("last_played")
                .and_then(|v| v.as_str())
                .map(str::to_string)
            {
                self.last_played = Some(name);
            }
            if let Some(name) = chore
                .get("last_looping")
                .and_then(|v| v.as_str())
                .map(str::to_string)
            {
                self.last_looping = Some(name);
            }
            if let Some(name) = chore
                .get("last_completed")
                .and_then(|v| v.as_str())
                .map(str::to_string)
            {
                self.last_completed = Some(name);
            }
        }
    }

    fn apply_event(&mut self, method: &str, args: &[String], trigger: Option<String>) {
        if let Some(source) = trigger {
            if self.created_by.is_none() {
                self.created_by = Some(source);
            }
        }

        self.methods.insert(method.to_string());

        let lower = method.to_ascii_lowercase();
        match lower.as_str() {
            "setpos" | "set_pos" | "set_position" => {
                if let Some(vec) = parse_vec3_args(args) {
                    self.position = Some(vec);
                }
            }
            "setrot" | "set_rot" | "set_rotation" => {
                if let Some(vec) = parse_vec3_args(args) {
                    self.rotation = Some(vec);
                }
            }
            "set_face_target" | "set_facing" | "look_at" => {
                if let Some(target) = args.first() {
                    let trimmed = target.trim();
                    if !trimmed.is_empty() && trimmed != "<expr>" {
                        self.facing_target = Some(trimmed.to_string());
                    }
                }
            }
            "play_chore" => {
                if let Some(name) = args.first() {
                    self.last_played = Some(name.clone());
                }
            }
            "play_chore_looping" => {
                if let Some(name) = args.first() {
                    self.last_looping = Some(name.clone());
                    self.last_played = Some(name.clone());
                }
            }
            "complete_chore" => {
                if let Some(name) = args.first() {
                    self.last_completed = Some(name.clone());
                }
            }
            _ => {}
        }
    }

    fn build(self) -> SceneEntity {
        SceneEntity {
            kind: self.key.kind,
            name: self.key.name,
            created_by: self.created_by,
            methods: self.methods.into_iter().collect(),
            position: self.position,
            rotation: self.rotation,
            facing_target: self.facing_target,
            last_played: self.last_played,
            last_looping: self.last_looping,
            last_completed: self.last_completed,
        }
    }
}

#[derive(Debug)]
struct SceneEntity {
    kind: SceneEntityKind,
    name: String,
    created_by: Option<String>,
    methods: Vec<String>,
    position: Option<[f32; 3]>,
    rotation: Option<[f32; 3]>,
    facing_target: Option<String>,
    last_played: Option<String>,
    last_looping: Option<String>,
    last_completed: Option<String>,
}

impl SceneEntity {
    fn describe(&self) -> String {
        let mut method_list = self.methods.clone();
        method_list.sort();
        let methods_label = if method_list.is_empty() {
            Cow::Borrowed("no recorded methods")
        } else {
            let preview_len = method_list.len().min(5);
            let mut label = method_list[..preview_len].join(", ");
            if method_list.len() > preview_len {
                label.push_str(&format!(", +{} more", method_list.len() - preview_len));
            }
            Cow::Owned(label)
        };

        let header = format!("[{}] {}", self.kind.label(), self.name);
        match &self.created_by {
            Some(source) => format!("{header} ({methods}) <= {source}", methods = methods_label),
            None => format!("{header} ({methods})", methods = methods_label),
        }
    }
}

#[derive(Debug)]
struct SceneBounds {
    min: [f32; 3],
    max: [f32; 3],
}

impl SceneBounds {
    fn update(&mut self, position: [f32; 3]) {
        for axis in 0..3 {
            self.min[axis] = self.min[axis].min(position[axis]);
            self.max[axis] = self.max[axis].max(position[axis]);
        }
    }
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct MarkerVertex {
    position: [f32; 2],
}

#[repr(C, align(16))]
#[derive(Clone, Copy, Pod, Zeroable)]
struct MarkerInstance {
    translate: [f32; 2],
    size: f32,
    highlight: f32,
    color: [f32; 3],
    _padding: f32,
}

const MARKER_VERTICES: [MarkerVertex; 6] = [
    MarkerVertex {
        position: [-0.5, -0.5],
    },
    MarkerVertex {
        position: [0.5, -0.5],
    },
    MarkerVertex {
        position: [-0.5, 0.5],
    },
    MarkerVertex {
        position: [-0.5, 0.5],
    },
    MarkerVertex {
        position: [0.5, -0.5],
    },
    MarkerVertex {
        position: [0.5, 0.5],
    },
];

fn load_scene_from_timeline(path: &Path) -> Result<ViewerScene> {
    let data = std::fs::read(path)
        .with_context(|| format!("reading timeline manifest {}", path.display()))?;
    let manifest: Value = serde_json::from_slice(&data)
        .with_context(|| format!("parsing timeline manifest {}", path.display()))?;

    let mut builders: BTreeMap<SceneEntityKey, SceneEntityBuilder> = BTreeMap::new();

    if let Some(engine_state) = manifest.get("engine_state") {
        if let Some(actor_map) = engine_state
            .get("replay_snapshot")
            .and_then(|replay| replay.get("actors"))
            .and_then(|actors| actors.as_object())
        {
            for (key, value) in actor_map {
                let name = value
                    .get("name")
                    .and_then(|v| v.as_str())
                    .unwrap_or(key)
                    .to_string();
                let entry = builders
                    .entry(SceneEntityKey::new(SceneEntityKind::Actor, name.clone()))
                    .or_insert_with(|| SceneEntityBuilder::new(SceneEntityKind::Actor, name));
                entry.apply_actor_snapshot(value);
            }
        }

        if let Some(events) = engine_state
            .get("subsystem_delta_events")
            .and_then(|v| v.as_array())
        {
            for event in events {
                let subsystem = event
                    .get("subsystem")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let name = match event.get("target").and_then(|v| v.as_str()) {
                    Some(name) if !name.is_empty() => name.to_string(),
                    _ => continue,
                };

                let kind = match subsystem {
                    "Objects" => SceneEntityKind::Object,
                    "InterestActors" => SceneEntityKind::InterestActor,
                    "Actors" => SceneEntityKind::Actor,
                    _ => continue,
                };

                let entry = builders
                    .entry(SceneEntityKey::new(kind, name.clone()))
                    .or_insert_with(|| SceneEntityBuilder::new(kind, name));

                let method = event.get("method").and_then(|v| v.as_str()).unwrap_or("");
                let args: Vec<String> = event
                    .get("arguments")
                    .and_then(|v| v.as_array())
                    .map(|values| {
                        values
                            .iter()
                            .filter_map(|value| value.as_str().map(str::to_string))
                            .collect()
                    })
                    .unwrap_or_default();
                let trigger = event.get("triggered_by").and_then(format_hook_reference);
                entry.apply_event(method, &args, trigger);
            }
        }
    }

    let mut entities: Vec<SceneEntity> = builders
        .into_iter()
        .map(|(_, builder)| builder.build())
        .collect();
    entities.sort_by(|a, b| a.kind.cmp(&b.kind).then_with(|| a.name.cmp(&b.name)));

    let mut bounds = None;
    for entity in &entities {
        if let Some(position) = entity.position {
            bounds
                .get_or_insert(SceneBounds {
                    min: position,
                    max: position,
                })
                .update(position);
        }
    }

    Ok(ViewerScene {
        entities,
        position_bounds: bounds,
    })
}

fn format_hook_reference(value: &Value) -> Option<String> {
    let hook_name = value.get("name")?.as_str()?;
    let defined_in = value
        .get("defined_in")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown.lua");
    let line_suffix = value
        .get("defined_at_line")
        .and_then(|v| v.as_u64())
        .map(|line| format!(":{}", line))
        .unwrap_or_default();
    let stage_label = value
        .get("stage")
        .and_then(|stage| stage.get("label"))
        .and_then(|label| label.as_str());

    match stage_label {
        Some(label) => Some(format!(
            "{} @ {}{} [{}]",
            hook_name, defined_in, line_suffix, label
        )),
        None => Some(format!("{} @ {}{}", hook_name, defined_in, line_suffix)),
    }
}

fn parse_vec3_object(value: &Value) -> Option<[f32; 3]> {
    let x = value.get("x")?.as_f64()? as f32;
    let y = value.get("y")?.as_f64()? as f32;
    let z = value.get("z")?.as_f64()? as f32;
    Some([x, y, z])
}

fn parse_vec3_args(args: &[String]) -> Option<[f32; 3]> {
    if args.len() < 3 {
        return None;
    }
    let mut values = [0.0f32; 3];
    for (idx, slot) in values.iter_mut().enumerate() {
        let value = args[idx].trim();
        if value == "<expr>" {
            return None;
        }
        *slot = parse_f32(value)?;
    }
    Some(values)
}

fn parse_f32(value: &str) -> Option<f32> {
    let trimmed = value.trim().trim_matches('"');
    trimmed.parse::<f32>().ok()
}

fn load_asset_bytes(manifest_path: &Path, asset: &str) -> Result<(String, Vec<u8>, PathBuf)> {
    let data = std::fs::read(manifest_path)
        .with_context(|| format!("reading asset manifest {}", manifest_path.display()))?;
    let manifest: AssetManifest = serde_json::from_slice(&data)
        .with_context(|| format!("parsing asset manifest {}", manifest_path.display()))?;

    let entry = manifest
        .found
        .into_iter()
        .find(|entry| entry.asset_name.eq_ignore_ascii_case(asset))
        .ok_or_else(|| {
            anyhow!(
                "asset '{}' not listed in manifest {}",
                asset,
                manifest_path.display()
            )
        })?;

    if let Some(AssetMetadataSummary::Bitmap {
        codec, supported, ..
    }) = &entry.metadata
    {
        if !supported {
            bail!(
                "asset '{}' (codec {}) is not yet supported by the viewer; pick a classic-surface entry",
                entry.asset_name,
                codec
            );
        }
    }

    let archive_path = resolve_archive_path(manifest_path, &entry.archive_path);
    let bytes = read_asset_slice(&archive_path, entry.offset, entry.size).with_context(|| {
        format!(
            "reading {} from {}",
            entry.asset_name,
            archive_path.display()
        )
    })?;

    Ok((entry.asset_name, bytes, archive_path))
}

fn resolve_archive_path(manifest_path: &Path, archive_path: &Path) -> PathBuf {
    if archive_path.is_absolute() {
        return archive_path.to_path_buf();
    }

    let from_manifest = manifest_path
        .parent()
        .map(|parent| parent.join(archive_path))
        .unwrap_or_else(|| archive_path.to_path_buf());
    if from_manifest.exists() {
        return from_manifest;
    }

    if archive_path.exists() {
        return archive_path.to_path_buf();
    }

    from_manifest
}

fn read_asset_slice(path: &Path, offset: u64, size: u32) -> Result<Vec<u8>> {
    let mut file = File::open(path).with_context(|| format!("opening {}", path.display()))?;
    file.seek(SeekFrom::Start(offset))
        .with_context(|| format!("seeking to 0x{:X} in {}", offset, path.display()))?;

    let mut buffer = vec![0u8; size as usize];
    file.read_exact(&mut buffer)
        .with_context(|| format!("reading {} bytes from {}", size, path.display()))?;
    Ok(buffer)
}

struct ViewerState {
    window: Arc<Window>,
    surface: wgpu::Surface<'static>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    config: wgpu::SurfaceConfiguration,
    size: winit::dpi::PhysicalSize<u32>,
    pipeline: wgpu::RenderPipeline,
    quad_vertex_buffer: wgpu::Buffer,
    quad_index_buffer: wgpu::Buffer,
    quad_index_count: u32,
    bind_group: wgpu::BindGroup,
    _texture: wgpu::Texture,
    _texture_view: wgpu::TextureView,
    _sampler: wgpu::Sampler,
    background: wgpu::Color,
    scene: Option<Arc<ViewerScene>>,
    selected_entity: Option<usize>,
    marker_pipeline: wgpu::RenderPipeline,
    marker_vertex_buffer: wgpu::Buffer,
    marker_instance_buffer: wgpu::Buffer,
    marker_capacity: usize,
}

impl ViewerState {
    async fn new(
        window: Arc<Window>,
        asset_name: &str,
        asset_bytes: Vec<u8>,
        decode_result: Result<PreviewTexture>,
        scene: Option<Arc<ViewerScene>>,
    ) -> Result<Self> {
        let size = window.inner_size();

        let instance = wgpu::Instance::default();
        let surface = instance
            .create_surface(window.clone())
            .context("creating wgpu surface")?;

        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::HighPerformance,
                force_fallback_adapter: false,
                compatible_surface: Some(&surface),
            })
            .await
            .context("requesting wgpu adapter")?;

        let (device, queue) = adapter
            .request_device(
                &wgpu::DeviceDescriptor {
                    label: Some("grim-viewer-device"),
                    required_features: wgpu::Features::empty(),
                    required_limits: wgpu::Limits::default(),
                },
                None,
            )
            .await
            .context("requesting wgpu device")?;

        let surface_caps = surface.get_capabilities(&adapter);
        let surface_format = surface_caps
            .formats
            .iter()
            .copied()
            .find(|format| format.is_srgb())
            .unwrap_or(surface_caps.formats[0]);
        let present_mode = surface_caps
            .present_modes
            .iter()
            .copied()
            .find(|mode| *mode == wgpu::PresentMode::Mailbox)
            .or(Some(wgpu::PresentMode::Fifo))
            .unwrap_or(wgpu::PresentMode::Fifo);
        let alpha_mode = surface_caps
            .alpha_modes
            .first()
            .copied()
            .unwrap_or(wgpu::CompositeAlphaMode::Opaque);

        let (preview, background) = match decode_result {
            Ok(texture) => {
                println!(
                    "Decoded BM frame: {}x{} ({} frames, codec {})",
                    texture.width, texture.height, texture.frame_count, texture.codec
                );
                (texture, wgpu::Color::BLACK)
            }
            Err(err) => {
                eprintln!("[grim_viewer] falling back to placeholder texture: {err:?}");
                let placeholder = generate_placeholder_texture(&asset_bytes, asset_name);
                let color = preview_color(&asset_bytes);
                (placeholder, color)
            }
        };
        let texture_width = preview.width;
        let texture_height = preview.height;
        let texture_extent = wgpu::Extent3d {
            width: texture_width,
            height: texture_height,
            depth_or_array_layers: 1,
        };

        println!(
            "Preview texture sized {}x{} ({} bytes of source)",
            texture_width,
            texture_height,
            asset_bytes.len()
        );

        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("asset-texture"),
            size: texture_extent,
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8UnormSrgb,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        queue.write_texture(
            wgpu::ImageCopyTexture {
                texture: &texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            &preview.data,
            wgpu::ImageDataLayout {
                offset: 0,
                bytes_per_row: Some(4 * texture_width),
                rows_per_image: Some(texture_height),
            },
            texture_extent,
        );
        let texture_view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("asset-sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Nearest,
            min_filter: wgpu::FilterMode::Nearest,
            mipmap_filter: wgpu::FilterMode::Nearest,
            ..Default::default()
        });

        let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("asset-bind-group-layout"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
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
            label: Some("asset-bind-group"),
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
            label: Some("asset-shader"),
            source: wgpu::ShaderSource::Wgsl(Cow::Borrowed(SHADER_SOURCE)),
        });

        let quad_vertex_layout = wgpu::VertexBufferLayout {
            array_stride: std::mem::size_of::<QuadVertex>() as u64,
            step_mode: wgpu::VertexStepMode::Vertex,
            attributes: &wgpu::vertex_attr_array![0 => Float32x2, 1 => Float32x2],
        };

        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("asset-pipeline-layout"),
            bind_group_layouts: &[&bind_group_layout],
            push_constant_ranges: &[],
        });

        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("asset-pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: "vs_main",
                buffers: &[quad_vertex_layout],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: "fs_main",
                targets: &[Some(wgpu::ColorTargetState {
                    format: surface_format,
                    blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            }),
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview: None,
        });

        let quad_vertex_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("asset-quad-vertex-buffer"),
            contents: cast_slice(&QUAD_VERTICES),
            usage: wgpu::BufferUsages::VERTEX,
        });
        let quad_index_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("asset-quad-index-buffer"),
            contents: cast_slice(&QUAD_INDICES),
            usage: wgpu::BufferUsages::INDEX,
        });
        let quad_index_count = QUAD_INDICES.len() as u32;

        let marker_vertex_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("marker-vertex-buffer"),
            contents: cast_slice(&MARKER_VERTICES),
            usage: wgpu::BufferUsages::VERTEX,
        });

        let initial_marker_capacity = 4usize;
        let marker_instance_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("marker-instance-buffer"),
            size: (initial_marker_capacity * std::mem::size_of::<MarkerInstance>()) as u64,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let marker_vertex_layout = wgpu::VertexBufferLayout {
            array_stride: std::mem::size_of::<MarkerVertex>() as u64,
            step_mode: wgpu::VertexStepMode::Vertex,
            attributes: &wgpu::vertex_attr_array![0 => Float32x2],
        };

        let marker_instance_layout = wgpu::VertexBufferLayout {
            array_stride: std::mem::size_of::<MarkerInstance>() as u64,
            step_mode: wgpu::VertexStepMode::Instance,
            attributes: &[
                wgpu::VertexAttribute {
                    offset: 0,
                    shader_location: 1,
                    format: wgpu::VertexFormat::Float32x2,
                },
                wgpu::VertexAttribute {
                    offset: 8,
                    shader_location: 2,
                    format: wgpu::VertexFormat::Float32,
                },
                wgpu::VertexAttribute {
                    offset: 12,
                    shader_location: 3,
                    format: wgpu::VertexFormat::Float32,
                },
                wgpu::VertexAttribute {
                    offset: 16,
                    shader_location: 4,
                    format: wgpu::VertexFormat::Float32x3,
                },
            ],
        };

        let marker_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("marker-shader"),
            source: wgpu::ShaderSource::Wgsl(Cow::Borrowed(MARKER_SHADER_SOURCE)),
        });

        let marker_pipeline_layout =
            device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("marker-pipeline-layout"),
                bind_group_layouts: &[],
                push_constant_ranges: &[],
            });

        let marker_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("marker-pipeline"),
            layout: Some(&marker_pipeline_layout),
            vertex: wgpu::VertexState {
                module: &marker_shader,
                entry_point: "vs_main",
                buffers: &[marker_vertex_layout, marker_instance_layout],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &marker_shader,
                entry_point: "fs_main",
                targets: &[Some(wgpu::ColorTargetState {
                    format: surface_format,
                    blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            }),
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview: None,
        });

        let selected_entity = scene.as_ref().and_then(|scene| {
            if scene.entities.is_empty() {
                None
            } else {
                Some(
                    scene
                        .entities
                        .iter()
                        .enumerate()
                        .find(|(_, entity)| entity.position.is_some())
                        .map(|(idx, _)| idx)
                        .unwrap_or(0),
                )
            }
        });

        let state = Self {
            window,
            surface,
            device,
            queue,
            config: wgpu::SurfaceConfiguration {
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
                format: surface_format,
                width: size.width.max(1),
                height: size.height.max(1),
                present_mode,
                alpha_mode,
                view_formats: vec![],
                desired_maximum_frame_latency: 1,
            },
            size,
            pipeline,
            quad_vertex_buffer,
            quad_index_buffer,
            quad_index_count,
            bind_group,
            _texture: texture,
            _texture_view: texture_view,
            _sampler: sampler,
            background,
            scene: scene.clone(),
            selected_entity,
            marker_pipeline,
            marker_vertex_buffer,
            marker_instance_buffer,
            marker_capacity: initial_marker_capacity,
        };

        state.surface.configure(&state.device, &state.config);
        state.print_selected_entity();

        Ok(state)
    }

    fn window(&self) -> &Window {
        self.window.as_ref()
    }

    fn size(&self) -> winit::dpi::PhysicalSize<u32> {
        self.size
    }

    fn resize(&mut self, new_size: winit::dpi::PhysicalSize<u32>) {
        if new_size.width > 0 && new_size.height > 0 {
            self.size = new_size;
            self.config.width = new_size.width;
            self.config.height = new_size.height;
            self.surface.configure(&self.device, &self.config);
        }
    }

    fn render(&mut self) -> Result<(), SurfaceError> {
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
            let mut rpass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("grim-viewer-pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(self.background),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });
            rpass.set_pipeline(&self.pipeline);
            rpass.set_bind_group(0, &self.bind_group, &[]);
            rpass.set_vertex_buffer(0, self.quad_vertex_buffer.slice(..));
            rpass.set_index_buffer(self.quad_index_buffer.slice(..), wgpu::IndexFormat::Uint16);
            rpass.draw_indexed(0..self.quad_index_count, 0, 0..1);
        }

        let marker_instances = self.build_marker_instances();
        if !marker_instances.is_empty() {
            self.ensure_marker_capacity(marker_instances.len());
            self.queue.write_buffer(
                &self.marker_instance_buffer,
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
            marker_pass.set_pipeline(&self.marker_pipeline);
            marker_pass.set_vertex_buffer(0, self.marker_vertex_buffer.slice(..));
            let instance_byte_len =
                (marker_instances.len() * std::mem::size_of::<MarkerInstance>()) as u64;
            marker_pass
                .set_vertex_buffer(1, self.marker_instance_buffer.slice(0..instance_byte_len));
            marker_pass.draw(
                0..MARKER_VERTICES.len() as u32,
                0..marker_instances.len() as u32,
            );
        }

        self.queue.submit(std::iter::once(encoder.finish()));
        frame.present();
        Ok(())
    }

    fn next_entity(&mut self) {
        if let Some(scene) = self.scene.as_ref() {
            if scene.entities.is_empty() {
                return;
            }
            let next = match self.selected_entity {
                Some(idx) => (idx + 1) % scene.entities.len(),
                None => 0,
            };
            self.selected_entity = Some(next);
            self.print_selected_entity();
        }
    }

    fn previous_entity(&mut self) {
        if let Some(scene) = self.scene.as_ref() {
            if scene.entities.is_empty() {
                return;
            }
            let prev = match self.selected_entity {
                Some(0) | None => scene.entities.len().saturating_sub(1),
                Some(idx) => idx.saturating_sub(1),
            };
            self.selected_entity = Some(prev);
            self.print_selected_entity();
        }
    }

    fn print_selected_entity(&self) {
        if let (Some(scene), Some(idx)) = (self.scene.as_ref(), self.selected_entity) {
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
            }
        }
    }

    fn build_marker_instances(&self) -> Vec<MarkerInstance> {
        let mut instances = Vec::new();

        let scene = match self.scene.as_ref() {
            Some(scene) => scene,
            None => return instances,
        };

        let bounds = match scene.position_bounds.as_ref() {
            Some(bounds) => bounds,
            None => return instances,
        };

        let width = (bounds.max[0] - bounds.min[0]).max(0.001);
        let depth = (bounds.max[2] - bounds.min[2]).max(0.001);
        let selected = self.selected_entity;

        for (idx, entity) in scene.entities.iter().enumerate() {
            let position = match entity.position {
                Some(pos) => pos,
                None => continue,
            };

            let norm_x = (position[0] - bounds.min[0]) / width;
            let norm_z = (position[2] - bounds.min[2]) / depth;
            let ndc_x = norm_x.clamp(0.0, 1.0) * 2.0 - 1.0;
            let ndc_y = 1.0 - norm_z.clamp(0.0, 1.0) * 2.0;

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
            let color = if is_selected {
                [0.95, 0.35, 0.25]
            } else {
                match entity.kind {
                    SceneEntityKind::Actor => [0.2, 0.85, 0.6],
                    SceneEntityKind::Object => [0.25, 0.6, 0.95],
                    SceneEntityKind::InterestActor => [0.85, 0.7, 0.25],
                }
            };

            instances.push(MarkerInstance {
                translate: [ndc_x, ndc_y],
                size,
                highlight: if is_selected { 1.0 } else { 0.0 },
                color,
                _padding: 0.0,
            });
        }

        instances
    }

    fn ensure_marker_capacity(&mut self, required: usize) {
        if required <= self.marker_capacity {
            return;
        }

        let new_capacity = required.next_power_of_two().max(4);
        let new_size = (new_capacity * std::mem::size_of::<MarkerInstance>()) as u64;
        self.marker_instance_buffer = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("marker-instance-buffer"),
            size: new_size,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        self.marker_capacity = new_capacity;
    }
}
struct PreviewTexture {
    data: Vec<u8>,
    width: u32,
    height: u32,
    frame_count: u32,
    codec: u32,
}

fn decode_asset_texture(asset_name: &str, bytes: &[u8]) -> Result<PreviewTexture> {
    let lower = asset_name.to_ascii_lowercase();
    if !(lower.ends_with(".bm") || lower.ends_with(".zbm")) {
        bail!("asset {asset_name} is not a BM surface");
    }

    let bm = decode_bm(bytes)?;
    let frame = bm
        .frames
        .first()
        .ok_or_else(|| anyhow!("BM surface has no frames"))?;
    let rgba = frame.as_rgba8888(bm.bits_per_pixel)?;
    Ok(PreviewTexture {
        data: rgba,
        width: frame.width,
        height: frame.height,
        frame_count: bm.image_count,
        codec: bm.codec,
    })
}

struct TextureStats {
    min_luma: u8,
    max_luma: u8,
    mean_luma: f32,
    opaque_pixels: u32,
    total_pixels: u32,
    quadrant_means: [f32; 4],
}

struct RenderVerification {
    stats: TextureStats,
    diff: TextureDiffSummary,
}

struct TextureDiffSummary {
    total_pixels: u32,
    mismatched_pixels: u32,
    max_abs_diff: u8,
    mean_abs_diff: f32,
    quadrant_mismatch: [f32; 4],
}

fn diff_mismatch_ratio(diff: &TextureDiffSummary) -> f32 {
    if diff.total_pixels == 0 {
        0.0
    } else {
        diff.mismatched_pixels as f32 / diff.total_pixels as f32
    }
}

fn validate_render_diff(diff: &TextureDiffSummary, threshold: f32) -> Result<()> {
    ensure!(
        (0.0..=1.0).contains(&threshold),
        "render diff threshold must be between 0 and 1 (got {})",
        threshold
    );
    let ratio = diff_mismatch_ratio(diff);
    if ratio > threshold {
        bail!(
            "post-render image diverges from decoded bitmap beyond allowed threshold ({:.4} > {:.4})",
            ratio,
            threshold
        );
    }
    Ok(())
}

fn dump_texture_to_png(preview: &PreviewTexture, destination: &Path) -> Result<TextureStats> {
    export_rgba_to_png(preview.width, preview.height, &preview.data, destination)
}

fn export_rgba_to_png(
    width: u32,
    height: u32,
    data: &[u8],
    destination: &Path,
) -> Result<TextureStats> {
    let expected_len = width as usize * height as usize * 4;
    ensure!(
        data.len() == expected_len,
        "RGBA buffer size {} does not match dimensions {}x{}",
        data.len(),
        width,
        height
    );

    let file = File::create(destination)?;
    let encoder = PngEncoder::new(file);
    encoder.write_image(data, width, height, ColorType::Rgba8.into())?;

    Ok(compute_texture_stats(width, height, data))
}

fn compute_texture_stats(width: u32, height: u32, data: &[u8]) -> TextureStats {
    let mut min_luma = u8::MAX;
    let mut max_luma = u8::MIN;
    let mut sum_luma: u64 = 0;
    let mut total_pixels: u32 = 0;
    let mut opaque_pixels: u32 = 0;
    let mut quadrant_sums = [0u64; 4];
    let mut quadrant_counts = [0u32; 4];

    let half_h = height / 2;
    let half_w = width / 2;

    for (idx, chunk) in data.chunks(4).enumerate() {
        let r = chunk[0] as u16;
        let g = chunk[1] as u16;
        let b = chunk[2] as u16;
        let a = chunk[3];
        let luma = ((r + g + b) / 3) as u8;
        min_luma = min_luma.min(luma);
        max_luma = max_luma.max(luma);
        sum_luma += luma as u64;
        total_pixels += 1;
        if a > 0 {
            opaque_pixels += 1;
        }

        let px = idx as u32 % width;
        let py = idx as u32 / width;
        let quadrant = match (px < half_w, py < half_h) {
            (true, true) => 0,   // top-left
            (false, true) => 1,  // top-right
            (true, false) => 2,  // bottom-left
            (false, false) => 3, // bottom-right
        };
        quadrant_sums[quadrant] += luma as u64;
        quadrant_counts[quadrant] += 1;
    }

    let mean_luma = if total_pixels > 0 {
        (sum_luma as f64 / total_pixels as f64) as f32
    } else {
        0.0
    };

    let mut quadrant_means = [0.0; 4];
    for idx in 0..4 {
        quadrant_means[idx] = if quadrant_counts[idx] > 0 {
            (quadrant_sums[idx] as f64 / quadrant_counts[idx] as f64) as f32
        } else {
            0.0
        };
    }

    TextureStats {
        min_luma,
        max_luma,
        mean_luma,
        opaque_pixels,
        total_pixels,
        quadrant_means,
    }
}

fn render_texture_offscreen(
    preview: &PreviewTexture,
    destination: Option<&Path>,
) -> Result<RenderVerification> {
    let instance = wgpu::Instance::new(InstanceDescriptor {
        backends: Backends::all(),
        flags: InstanceFlags::default(),
        ..Default::default()
    });
    let adapter = instance
        .request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            force_fallback_adapter: false,
            compatible_surface: None,
        })
        .block_on()
        .or_else(|| {
            instance
                .request_adapter(&wgpu::RequestAdapterOptions {
                    power_preference: wgpu::PowerPreference::LowPower,
                    force_fallback_adapter: false,
                    compatible_surface: None,
                })
                .block_on()
        })
        .or_else(|| {
            instance
                .request_adapter(&wgpu::RequestAdapterOptions {
                    power_preference: wgpu::PowerPreference::LowPower,
                    force_fallback_adapter: true,
                    compatible_surface: None,
                })
                .block_on()
        })
        .context("requesting adapter for offscreen render")?;

    let (device, queue) = adapter
        .request_device(
            &wgpu::DeviceDescriptor {
                label: Some("grim-viewer-offscreen-device"),
                required_features: wgpu::Features::empty(),
                required_limits: wgpu::Limits::default(),
            },
            None,
        )
        .block_on()
        .context("requesting device for offscreen render")?;

    let texture_extent = wgpu::Extent3d {
        width: preview.width,
        height: preview.height,
        depth_or_array_layers: 1,
    };

    let asset_texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("offscreen-asset-texture"),
        size: texture_extent,
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8UnormSrgb,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    queue.write_texture(
        wgpu::ImageCopyTexture {
            texture: &asset_texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        &preview.data,
        wgpu::ImageDataLayout {
            offset: 0,
            bytes_per_row: Some(4 * preview.width),
            rows_per_image: Some(preview.height),
        },
        texture_extent,
    );

    let asset_view = asset_texture.create_view(&wgpu::TextureViewDescriptor::default());
    let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
        label: Some("offscreen-asset-sampler"),
        address_mode_u: wgpu::AddressMode::ClampToEdge,
        address_mode_v: wgpu::AddressMode::ClampToEdge,
        address_mode_w: wgpu::AddressMode::ClampToEdge,
        mag_filter: wgpu::FilterMode::Nearest,
        min_filter: wgpu::FilterMode::Nearest,
        mipmap_filter: wgpu::FilterMode::Nearest,
        ..Default::default()
    });

    let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("offscreen-bind-group-layout"),
        entries: &[
            wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Float { filterable: true },
                    view_dimension: wgpu::TextureViewDimension::D2,
                    multisampled: false,
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
        label: Some("offscreen-bind-group"),
        layout: &bind_group_layout,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(&asset_view),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: wgpu::BindingResource::Sampler(&sampler),
            },
        ],
    });

    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("offscreen-shader"),
        source: wgpu::ShaderSource::Wgsl(Cow::Borrowed(SHADER_SOURCE)),
    });

    let quad_vertex_layout = wgpu::VertexBufferLayout {
        array_stride: std::mem::size_of::<QuadVertex>() as u64,
        step_mode: wgpu::VertexStepMode::Vertex,
        attributes: &wgpu::vertex_attr_array![0 => Float32x2, 1 => Float32x2],
    };

    let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("offscreen-pipeline-layout"),
        bind_group_layouts: &[&bind_group_layout],
        push_constant_ranges: &[],
    });

    let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("offscreen-pipeline"),
        layout: Some(&pipeline_layout),
        vertex: wgpu::VertexState {
            module: &shader,
            entry_point: "vs_main",
            buffers: &[quad_vertex_layout],
            compilation_options: wgpu::PipelineCompilationOptions::default(),
        },
        fragment: Some(wgpu::FragmentState {
            module: &shader,
            entry_point: "fs_main",
            targets: &[Some(wgpu::ColorTargetState {
                format: wgpu::TextureFormat::Rgba8UnormSrgb,
                blend: Some(wgpu::BlendState::REPLACE),
                write_mask: wgpu::ColorWrites::ALL,
            })],
            compilation_options: wgpu::PipelineCompilationOptions::default(),
        }),
        primitive: wgpu::PrimitiveState {
            topology: wgpu::PrimitiveTopology::TriangleList,
            strip_index_format: None,
            front_face: wgpu::FrontFace::Ccw,
            cull_mode: None,
            unclipped_depth: false,
            polygon_mode: wgpu::PolygonMode::Fill,
            conservative: false,
        },
        depth_stencil: None,
        multisample: wgpu::MultisampleState {
            count: 1,
            mask: !0,
            alpha_to_coverage_enabled: false,
        },
        multiview: None,
    });

    let quad_vertex_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("offscreen-quad-vertex-buffer"),
        contents: cast_slice(&QUAD_VERTICES),
        usage: wgpu::BufferUsages::VERTEX,
    });
    let quad_index_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("offscreen-quad-index-buffer"),
        contents: cast_slice(&QUAD_INDICES),
        usage: wgpu::BufferUsages::INDEX,
    });
    let quad_index_count = QUAD_INDICES.len() as u32;

    let render_texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("offscreen-target"),
        size: texture_extent,
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8UnormSrgb,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let render_view = render_texture.create_view(&wgpu::TextureViewDescriptor::default());

    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("offscreen-encoder"),
    });

    {
        let mut rpass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("offscreen-pass"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &render_view,
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
        rpass.set_pipeline(&pipeline);
        rpass.set_bind_group(0, &bind_group, &[]);
        rpass.set_vertex_buffer(0, quad_vertex_buffer.slice(..));
        rpass.set_index_buffer(quad_index_buffer.slice(..), wgpu::IndexFormat::Uint16);
        rpass.draw_indexed(0..quad_index_count, 0, 0..1);
    }

    let bytes_per_row = 4 * preview.width;
    let padded_bytes_per_row = ((bytes_per_row + COPY_BYTES_PER_ROW_ALIGNMENT - 1)
        / COPY_BYTES_PER_ROW_ALIGNMENT)
        * COPY_BYTES_PER_ROW_ALIGNMENT;
    let buffer_size = padded_bytes_per_row as u64 * preview.height as u64;
    let readback_buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("offscreen-readback"),
        size: buffer_size,
        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });

    encoder.copy_texture_to_buffer(
        wgpu::ImageCopyTexture {
            texture: &render_texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        wgpu::ImageCopyBuffer {
            buffer: &readback_buffer,
            layout: wgpu::ImageDataLayout {
                offset: 0,
                bytes_per_row: Some(padded_bytes_per_row),
                rows_per_image: Some(preview.height),
            },
        },
        texture_extent,
    );

    queue.submit(std::iter::once(encoder.finish()));
    device.poll(Maintain::Wait);

    let buffer_slice = readback_buffer.slice(..);
    let (tx, rx) = mpsc::channel();
    buffer_slice.map_async(wgpu::MapMode::Read, move |result| {
        let _ = tx.send(result);
    });
    device.poll(Maintain::Wait);
    match rx
        .recv()
        .context("waiting for offscreen readback completion")?
    {
        Ok(()) => {}
        Err(err) => bail!("mapping offscreen readback buffer: {err}"),
    }
    let padded = buffer_slice.get_mapped_range();
    let mut rgba = vec![0u8; (preview.width * preview.height * 4) as usize];

    for row in 0..preview.height as usize {
        let src_offset = row * padded_bytes_per_row as usize;
        let dst_offset = row * bytes_per_row as usize;
        rgba[dst_offset..dst_offset + bytes_per_row as usize]
            .copy_from_slice(&padded[src_offset..src_offset + bytes_per_row as usize]);
    }
    drop(padded);
    readback_buffer.unmap();

    let diff = summarize_texture_diff(preview, &rgba)?;
    let stats = match destination {
        Some(path) => export_rgba_to_png(preview.width, preview.height, &rgba, path)?,
        None => compute_texture_stats(preview.width, preview.height, &rgba),
    };
    Ok(RenderVerification { stats, diff })
}

fn summarize_texture_diff(preview: &PreviewTexture, rendered: &[u8]) -> Result<TextureDiffSummary> {
    let expected_len = (preview.width * preview.height * 4) as usize;
    ensure!(
        rendered.len() == expected_len,
        "rendered RGBA buffer size {} does not match expected {}x{}",
        rendered.len(),
        preview.width,
        preview.height
    );

    let mut mismatched_pixels: u32 = 0;
    let mut max_abs_diff: u8 = 0;
    let mut sum_abs_diff: u64 = 0;
    let mut quadrant_counts = [0u32; 4];
    let mut quadrant_mismatches = [0u32; 4];

    let half_w = preview.width / 2;
    let half_h = preview.height / 2;

    for (idx, (expected, actual)) in preview.data.chunks(4).zip(rendered.chunks(4)).enumerate() {
        let mut pixel_abs_diff: u8 = 0;
        for channel in 0..4 {
            let diff = u8::abs_diff(expected[channel], actual[channel]);
            if diff > pixel_abs_diff {
                pixel_abs_diff = diff;
            }
        }

        let px = idx as u32 % preview.width;
        let py = idx as u32 / preview.width;
        let quadrant = match (px < half_w, py < half_h) {
            (true, true) => 0,
            (false, true) => 1,
            (true, false) => 2,
            (false, false) => 3,
        };
        quadrant_counts[quadrant] += 1;

        if pixel_abs_diff > 0 {
            mismatched_pixels += 1;
            quadrant_mismatches[quadrant] += 1;
            max_abs_diff = max_abs_diff.max(pixel_abs_diff);
        }
        sum_abs_diff += pixel_abs_diff as u64;
    }

    let total_pixels = preview.width * preview.height;
    let mean_abs_diff = if total_pixels > 0 {
        (sum_abs_diff as f64 / total_pixels as f64) as f32
    } else {
        0.0
    };

    let mut quadrant_mismatch = [0.0_f32; 4];
    for idx in 0..4 {
        quadrant_mismatch[idx] = if quadrant_counts[idx] > 0 {
            quadrant_mismatches[idx] as f32 / quadrant_counts[idx] as f32
        } else {
            0.0
        };
    }

    Ok(TextureDiffSummary {
        total_pixels,
        mismatched_pixels,
        max_abs_diff,
        mean_abs_diff,
        quadrant_mismatch,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_diff(total: u32, mismatched: u32) -> TextureDiffSummary {
        TextureDiffSummary {
            total_pixels: total,
            mismatched_pixels: mismatched,
            max_abs_diff: 5,
            mean_abs_diff: 0.1,
            quadrant_mismatch: [0.0; 4],
        }
    }

    #[test]
    fn validate_render_diff_allows_within_threshold() {
        let diff = make_diff(10_000, 50); // 0.5%
        assert!(validate_render_diff(&diff, 0.01).is_ok());
    }

    #[test]
    fn validate_render_diff_rejects_exceeding_threshold() {
        let diff = make_diff(10_000, 5_000); // 50%
        let err = validate_render_diff(&diff, 0.25)
            .expect_err("expected failure when ratio exceeds threshold");
        assert!(
            err.to_string()
                .contains("post-render image diverges from decoded bitmap")
        );
    }

    #[test]
    fn validate_render_diff_rejects_invalid_threshold() {
        let diff = make_diff(10_000, 0);
        let err =
            validate_render_diff(&diff, 1.1).expect_err("threshold outside [0,1] should fail");
        assert!(
            err.to_string()
                .contains("render diff threshold must be between 0 and 1")
        );
    }
}

fn generate_placeholder_texture(bytes: &[u8], asset_name: &str) -> PreviewTexture {
    const WIDTH: u32 = 256;
    const HEIGHT: u32 = 256;
    let mut data = vec![0u8; (WIDTH * HEIGHT * 4) as usize];
    let len = bytes.len().max(1);
    let seed = asset_name
        .as_bytes()
        .iter()
        .fold(0u8, |acc, &b| acc.wrapping_add(b));

    for (idx, pixel) in data.chunks_mut(4).enumerate() {
        let base = (idx + seed as usize) % len;
        let r = bytes.get(base).copied().unwrap_or(seed);
        let g = bytes.get((base + 17) % len).copied().unwrap_or(r);
        let b = bytes.get((base + 43) % len).copied().unwrap_or(g);
        pixel[0] = r;
        pixel[1] = g;
        pixel[2] = b;
        pixel[3] = 0xFF;
    }

    PreviewTexture {
        data,
        width: WIDTH,
        height: HEIGHT,
        frame_count: 0,
        codec: 0,
    }
}

const SHADER_SOURCE: &str = r#"
struct VertexInput {
    @location(0) position: vec2<f32>,
    @location(1) uv: vec2<f32>,
};

struct VertexOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn vs_main(input: VertexInput) -> VertexOutput {
    var out: VertexOutput;
    out.position = vec4<f32>(input.position, 0.0, 1.0);
    out.uv = input.uv;
    return out;
}

@group(0) @binding(0)
var asset_texture: texture_2d<f32>;
@group(0) @binding(1)
var asset_sampler: sampler;

@fragment
fn fs_main(input: VertexOutput) -> @location(0) vec4<f32> {
    let uv = clamp(input.uv, vec2<f32>(0.0, 0.0), vec2<f32>(1.0, 1.0));
    return textureSample(asset_texture, asset_sampler, uv);
}
"#;

const MARKER_SHADER_SOURCE: &str = r#"
struct VertexOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) color: vec3<f32>,
};

struct VertexIn {
    @location(0) base_pos: vec2<f32>,
    @location(1) translate: vec2<f32>,
    @location(2) size: f32,
    @location(3) highlight: f32,
    @location(4) color: vec3<f32>,
};

@vertex
fn vs_main(input: VertexIn) -> VertexOutput {
    let scale = input.size * (1.0 + input.highlight * 0.6);
    let position = input.base_pos * scale + input.translate;
    var out: VertexOutput;
    out.position = vec4<f32>(position, 0.0, 1.0);
    out.color = input.color;
    return out;
}

@fragment
fn fs_main(input: VertexOutput) -> @location(0) vec4<f32> {
    return vec4<f32>(input.color, 0.9);
}
"#;

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct QuadVertex {
    position: [f32; 2],
    uv: [f32; 2],
}

const QUAD_VERTICES: [QuadVertex; 4] = [
    QuadVertex {
        position: [-1.0, 1.0],
        uv: [0.0, 0.0],
    },
    QuadVertex {
        position: [1.0, 1.0],
        uv: [1.0, 0.0],
    },
    QuadVertex {
        position: [-1.0, -1.0],
        uv: [0.0, 1.0],
    },
    QuadVertex {
        position: [1.0, -1.0],
        uv: [1.0, 1.0],
    },
];

const QUAD_INDICES: [u16; 6] = [0, 1, 2, 2, 1, 3];

fn preview_color(bytes: &[u8]) -> wgpu::Color {
    if bytes.is_empty() {
        return wgpu::Color::BLACK;
    }

    let mut hash = 0u64;
    for chunk in bytes.chunks(8) {
        let mut padded = [0u8; 8];
        for (idx, value) in chunk.iter().enumerate() {
            padded[idx] = *value;
        }
        hash ^= u64::from_le_bytes(padded).rotate_left(7);
    }

    let r = ((hash >> 0) & 0xFF) as f64 / 255.0;
    let g = ((hash >> 8) & 0xFF) as f64 / 255.0;
    let b = ((hash >> 16) & 0xFF) as f64 / 255.0;

    wgpu::Color { r, g, b, a: 1.0 }
}

fn init_audio() -> Result<()> {
    #[cfg(feature = "audio")]
    {
        let (_stream, _stream_handle) = OutputStream::try_default()
            .context("initializing default audio output device via rodio")?;
        let _ = (_stream, _stream_handle);
    }

    Ok(())
}
