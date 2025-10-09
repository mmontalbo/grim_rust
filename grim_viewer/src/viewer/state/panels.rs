use super::super::overlays::TextOverlay;
use crate::ui_layout::{PanelKind, UiLayout};
use wgpu::{Device, Queue};
use winit::dpi::PhysicalSize;

pub struct ViewerOverlays {
    audio: Option<TextOverlay>,
    timeline: Option<TextOverlay>,
    scrubber: Option<TextOverlay>,
}

impl ViewerOverlays {
    pub fn new(
        audio: Option<TextOverlay>,
        timeline: Option<TextOverlay>,
        scrubber: Option<TextOverlay>,
    ) -> Self {
        Self {
            audio,
            timeline,
            scrubber,
        }
    }

    pub fn audio_mut(&mut self) -> Option<&mut TextOverlay> {
        self.audio.as_mut()
    }

    pub fn timeline_mut(&mut self) -> Option<&mut TextOverlay> {
        self.timeline.as_mut()
    }

    pub fn scrubber_mut(&mut self) -> Option<&mut TextOverlay> {
        self.scrubber.as_mut()
    }

    pub fn audio(&self) -> Option<&TextOverlay> {
        self.audio.as_ref()
    }

    pub fn timeline(&self) -> Option<&TextOverlay> {
        self.timeline.as_ref()
    }

    pub fn scrubber(&self) -> Option<&TextOverlay> {
        self.scrubber.as_ref()
    }

    pub fn apply_layouts(
        &mut self,
        device: &Device,
        layout: &UiLayout,
        window_size: PhysicalSize<u32>,
    ) {
        if let Some(overlay) = self.audio.as_mut() {
            if let Some(rect) = layout.panel_rect(PanelKind::Audio) {
                overlay.update_layout(device, window_size, rect);
            }
        }
        if let Some(overlay) = self.timeline.as_mut() {
            if let Some(rect) = layout.panel_rect(PanelKind::Timeline) {
                overlay.update_layout(device, window_size, rect);
            }
        }
        if let Some(overlay) = self.scrubber.as_mut() {
            if let Some(rect) = layout.panel_rect(PanelKind::Scrubber) {
                overlay.update_layout(device, window_size, rect);
            }
        }
    }

    pub fn upload_visible(&mut self, queue: &Queue) {
        if let Some(overlay) = self.audio.as_mut() {
            overlay.upload(queue);
        }
        if let Some(overlay) = self.timeline.as_mut() {
            overlay.upload(queue);
        }
        if let Some(overlay) = self.scrubber.as_mut() {
            overlay.upload(queue);
        }
    }
}
