use std::path::{Path, PathBuf};

use anyhow::Result;

use crate::scene::{
    load_hotspot_event_log, load_lua_geometry_snapshot, load_movement_trace,
    load_scene_from_timeline, print_scene_summary, MovementTrace, ViewerScene,
};

#[derive(Debug, Clone)]
pub struct LiveSceneConfig {
    pub assets_manifest: PathBuf,
    pub timeline_manifest: PathBuf,
    pub geometry_snapshot: Option<PathBuf>,
    pub movement_log: Option<PathBuf>,
    pub hotspot_log: Option<PathBuf>,
    pub active_asset: Option<String>,
}

impl LiveSceneConfig {
    pub fn from_args(args: &crate::Args) -> Result<Option<Self>> {
        let repo_root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..");

        let assets_manifest = match args
            .scene_assets_manifest
            .clone()
            .or_else(|| locate(&repo_root, &["artifacts/manny_office_assets.json"]))
        {
            Some(path) => path,
            None => return Ok(None),
        };

        let timeline_manifest = match args
            .scene_timeline
            .clone()
            .or_else(|| {
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

        let movement_log = args
            .scene_movement_log
            .clone()
            .or_else(|| {
                locate(
                    &repo_root,
                    &[
                        "artifacts/run_cache/manny_movement_log.json",
                        "tools/tests/movement_log.json",
                    ],
                )
            });

        let hotspot_log = args
            .scene_hotspot_log
            .clone()
            .or_else(|| {
                locate(
                    &repo_root,
                    &[
                        "artifacts/run_cache/manny_hotspot_events.json",
                        "tools/tests/hotspot_events.json",
                    ],
                )
            });

        let active_asset = args.scene_active_asset.clone().or_else(|| {
            Some(String::from("mo_0_ddtws.bm"))
        });

        Ok(Some(Self {
            assets_manifest,
            timeline_manifest,
            geometry_snapshot,
            movement_log,
            hotspot_log,
            active_asset,
        }))
    }
}

pub struct LiveSceneState {
    scene: ViewerScene,
    _movement_trace: Option<MovementTrace>,
}

impl LiveSceneState {
    pub fn load(config: LiveSceneConfig) -> Result<Self> {
        let geometry = match config.geometry_snapshot.as_ref() {
            Some(path) => Some(load_lua_geometry_snapshot(path)?),
            None => None,
        };

        let mut scene = load_scene_from_timeline(
            &config.timeline_manifest,
            &config.assets_manifest,
            config.active_asset.as_deref(),
            geometry.as_ref(),
        )?;

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

        Ok(Self {
            scene,
            _movement_trace: movement_trace,
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
