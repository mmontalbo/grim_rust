//! Thin wrapper around the Taffy layout engine used to size the HUD panels and
//! minimap. The viewer provides panel hints, and this module keeps track of the
//! resulting rectangles whenever the window resizes so `viewer::state` can draw
//! overlays without sprinkling layout math throughout the render code.

use std::collections::HashMap;

use anyhow::{Context, Result};
use taffy::prelude::*;
use winit::dpi::PhysicalSize;

pub const PANEL_MARGIN: f32 = 16.0;
pub const DEFAULT_MINIMAP_MIN_SIDE: f32 = 160.0;
const DEFAULT_MINIMAP_PREFERRED_FRACTION: f32 = 0.3;
const DEFAULT_MINIMAP_MAX_FRACTION: f32 = 0.5;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ViewportRect {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PanelKind {
    Audio,
    Timeline,
    Scrubber,
    Minimap,
}

#[derive(Debug, Clone, Copy)]
pub struct PanelSize {
    pub width: f32,
    pub height: f32,
}

#[derive(Debug, Clone, Copy)]
pub struct MinimapConstraints {
    pub min_side: f32,
    pub preferred_fraction: f32,
    pub max_fraction: f32,
}

impl Default for MinimapConstraints {
    fn default() -> Self {
        Self {
            min_side: DEFAULT_MINIMAP_MIN_SIDE,
            preferred_fraction: DEFAULT_MINIMAP_PREFERRED_FRACTION,
            max_fraction: DEFAULT_MINIMAP_MAX_FRACTION,
        }
    }
}

pub struct UiLayout {
    tree: TaffyTree<()>,
    root: NodeId,
    panel_nodes: HashMap<PanelKind, NodeId>,
    window_size: PhysicalSize<u32>,
}

impl UiLayout {
    pub fn new(
        window_size: PhysicalSize<u32>,
        audio: Option<PanelSize>,
        timeline: Option<PanelSize>,
        scrubber: Option<PanelSize>,
        minimap: MinimapConstraints,
    ) -> Result<Self> {
        let mut tree = TaffyTree::new();
        let mut panel_nodes: HashMap<PanelKind, NodeId> = HashMap::new();
        let mut children: Vec<NodeId> = Vec::new();

        if let Some(size) = audio {
            let node = tree
                .new_leaf(Style {
                    position: Position::Absolute,
                    inset: Rect {
                        left: LengthPercentageAuto::Length(PANEL_MARGIN),
                        right: LengthPercentageAuto::Auto,
                        top: LengthPercentageAuto::Length(PANEL_MARGIN),
                        bottom: LengthPercentageAuto::Auto,
                    },
                    size: Size {
                        width: Dimension::Length(size.width),
                        height: Dimension::Length(size.height),
                    },
                    ..Default::default()
                })
                .context("creating audio panel node")?;
            panel_nodes.insert(PanelKind::Audio, node);
            children.push(node);
        }

        if let Some(size) = timeline {
            let node = tree
                .new_leaf(Style {
                    position: Position::Absolute,
                    inset: Rect {
                        left: LengthPercentageAuto::Auto,
                        right: LengthPercentageAuto::Length(PANEL_MARGIN),
                        top: LengthPercentageAuto::Length(PANEL_MARGIN),
                        bottom: LengthPercentageAuto::Auto,
                    },
                    size: Size {
                        width: Dimension::Length(size.width),
                        height: Dimension::Length(size.height),
                    },
                    ..Default::default()
                })
                .context("creating timeline panel node")?;
            panel_nodes.insert(PanelKind::Timeline, node);
            children.push(node);
        }

        if let Some(size) = scrubber {
            let node = tree
                .new_leaf(Style {
                    position: Position::Absolute,
                    inset: Rect {
                        left: LengthPercentageAuto::Length(PANEL_MARGIN),
                        right: LengthPercentageAuto::Auto,
                        top: LengthPercentageAuto::Auto,
                        bottom: LengthPercentageAuto::Length(PANEL_MARGIN),
                    },
                    size: Size {
                        width: Dimension::Length(size.width),
                        height: Dimension::Length(size.height),
                    },
                    ..Default::default()
                })
                .context("creating scrubber panel node")?;
            panel_nodes.insert(PanelKind::Scrubber, node);
            children.push(node);
        }

        let minimap_node = tree
            .new_leaf(Style {
                position: Position::Absolute,
                inset: Rect {
                    left: LengthPercentageAuto::Auto,
                    right: LengthPercentageAuto::Length(PANEL_MARGIN),
                    top: LengthPercentageAuto::Auto,
                    bottom: LengthPercentageAuto::Length(PANEL_MARGIN),
                },
                size: Size {
                    width: Dimension::Length(minimap.min_side),
                    height: Dimension::Length(minimap.min_side),
                },
                ..Default::default()
            })
            .context("creating minimap panel node")?;
        panel_nodes.insert(PanelKind::Minimap, minimap_node);
        children.push(minimap_node);

        let root = tree
            .new_with_children(
                Style {
                    size: Size {
                        width: Dimension::Length(window_size.width as f32),
                        height: Dimension::Length(window_size.height as f32),
                    },
                    ..Default::default()
                },
                &children,
            )
            .context("creating UI root node")?;

        tree.compute_layout(
            root,
            Size {
                width: AvailableSpace::Definite(window_size.width as f32),
                height: AvailableSpace::Definite(window_size.height as f32),
            },
        )
        .context("computing initial UI layout")?;

        Ok(Self {
            tree,
            root,
            panel_nodes,
            window_size,
        })
    }

    pub fn set_window_size(&mut self, size: PhysicalSize<u32>) -> Result<()> {
        if self.window_size == size {
            return Ok(());
        }
        self.window_size = size;
        let mut style = self
            .tree
            .style(self.root)
            .context("fetching root style for resize")?
            .clone();
        style.size = Size {
            width: Dimension::Length(size.width as f32),
            height: Dimension::Length(size.height as f32),
        };
        self.tree
            .set_style(self.root, style)
            .context("updating root style for resize")?;
        self.recompute()
    }

    pub fn recompute(&mut self) -> Result<()> {
        self.tree
            .compute_layout(
                self.root,
                Size {
                    width: AvailableSpace::Definite(self.window_size.width as f32),
                    height: AvailableSpace::Definite(self.window_size.height as f32),
                },
            )
            .context("recomputing UI layout")
    }

    pub fn panel_rect(&self, panel: PanelKind) -> Option<ViewportRect> {
        let node = *self.panel_nodes.get(&panel)?;
        let layout = self.tree.layout(node).ok()?;
        Some(ViewportRect {
            x: layout.location.x,
            y: layout.location.y,
            width: layout.size.width,
            height: layout.size.height,
        })
    }
}
