use std::mem::MaybeUninit;
use std::os::raw::c_char;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, anyhow};
use grim_formats::{SnmFile, blocky16::Blocky16Decoder};
use theorafile_rs::{
    OggTheora_File, tf_close, tf_eos, tf_fopen, tf_hasvideo, tf_readvideo, tf_videoinfo,
    th_pixel_fmt,
};

use crate::movie::yuv;

#[derive(Debug, Clone)]
pub enum MovieAsset {
    Snm(Arc<SnmFile>),
    Ogv(PathBuf),
}

pub trait MoviePlayback {
    fn name(&self) -> &str;
    fn width(&self) -> u32;
    fn height(&self) -> u32;
    fn frame_for_host_time(&mut self, host_time_ns: u64) -> Result<&[u8]>;
    fn status(&self) -> PlaybackStatus;
}

#[derive(Debug, Clone)]
pub struct PlaybackStatus {
    pub backend: &'static str,
    pub current_frame: Option<u64>,
    pub end_of_stream: bool,
}

pub enum Playback {
    Snm(SnmPlayback),
    Ogv(OgvPlayback),
}

impl Playback {
    pub fn new(name: String, asset: MovieAsset, start_host_time_ns: u64) -> Result<Self> {
        match asset {
            MovieAsset::Snm(snm) => Ok(Self::Snm(SnmPlayback::new(name, snm, start_host_time_ns)?)),
            MovieAsset::Ogv(path) => {
                Ok(Self::Ogv(OgvPlayback::new(name, path, start_host_time_ns)?))
            }
        }
    }
}

impl MoviePlayback for Playback {
    fn name(&self) -> &str {
        match self {
            Self::Snm(inner) => inner.name(),
            Self::Ogv(inner) => inner.name(),
        }
    }

    fn width(&self) -> u32 {
        match self {
            Self::Snm(inner) => inner.width(),
            Self::Ogv(inner) => inner.width(),
        }
    }

    fn height(&self) -> u32 {
        match self {
            Self::Snm(inner) => inner.height(),
            Self::Ogv(inner) => inner.height(),
        }
    }

    fn frame_for_host_time(&mut self, host_time_ns: u64) -> Result<&[u8]> {
        match self {
            Self::Snm(inner) => inner.frame_for_host_time(host_time_ns),
            Self::Ogv(inner) => inner.frame_for_host_time(host_time_ns),
        }
    }

    fn status(&self) -> PlaybackStatus {
        match self {
            Self::Snm(inner) => inner.status(),
            Self::Ogv(inner) => inner.status(),
        }
    }
}

pub struct SnmPlayback {
    name: String,
    snm: Arc<SnmFile>,
    decoder: Blocky16Decoder,
    rgba_buffer: Vec<u8>,
    current_frame: Option<u32>,
    start_host_time_ns: u64,
    frame_duration: Duration,
}

impl SnmPlayback {
    fn new(name: String, snm: Arc<SnmFile>, start_host_time_ns: u64) -> Result<Self> {
        let decoder = snm
            .blocky16_decoder()
            .with_context(|| format!("failed to create Blocky16 decoder for {}", name))?;
        let rgba_len = snm.blocky16_rgba_len();
        let rgba_buffer = vec![0u8; rgba_len.max(4)];
        let frame_duration = if snm.header.frame_rate > 0 {
            Duration::from_micros(snm.header.frame_rate as u64)
        } else {
            Duration::from_micros(33_333)
        };
        Ok(Self {
            name,
            snm,
            decoder,
            rgba_buffer,
            current_frame: None,
            start_host_time_ns,
            frame_duration,
        })
    }

    fn frame_count(&self) -> usize {
        self.snm.frames.len()
    }

    fn advance_to(&mut self, target: u32) -> Result<()> {
        if self.frame_count() == 0 {
            return Ok(());
        }
        let max_index = (self.frame_count() - 1) as u32;
        let clamped = target.min(max_index);
        if let Some(current) = self.current_frame {
            if current >= clamped {
                return Ok(());
            }
        }
        let start = self
            .current_frame
            .map(|value| value.saturating_add(1))
            .unwrap_or(0);
        if start > clamped {
            return Ok(());
        }
        for index in start..=clamped {
            let frame = &self.snm.frames[index as usize];
            let decoded = frame
                .decode_blocky16_rgba(&mut self.decoder, &mut self.rgba_buffer)
                .with_context(|| format!("failed to decode frame {} of {}", index, self.name))?;
            if !decoded && self.current_frame.is_none() && !self.rgba_buffer.is_empty() {
                self.rgba_buffer.fill(0);
            }
        }
        self.current_frame = Some(clamped);
        Ok(())
    }

    fn name(&self) -> &str {
        &self.name
    }

    fn width(&self) -> u32 {
        self.decoder.width() as u32
    }

    fn height(&self) -> u32 {
        self.decoder.height() as u32
    }

    fn frame_for_host_time(&mut self, host_time_ns: u64) -> Result<&[u8]> {
        if self.frame_count() == 0 {
            return Ok(&self.rgba_buffer);
        }
        let elapsed_ns = host_time_ns.saturating_sub(self.start_host_time_ns) as u128;
        let frame_duration_ns = self.frame_duration.as_nanos();
        let frame_idx = if frame_duration_ns > 0 {
            (elapsed_ns / frame_duration_ns) as u32
        } else {
            0
        };
        let max_index = (self.frame_count() - 1) as u32;
        self.advance_to(frame_idx.min(max_index))?;
        Ok(&self.rgba_buffer)
    }

    fn status(&self) -> PlaybackStatus {
        PlaybackStatus {
            backend: "snm",
            current_frame: self.current_frame.map(|value| value as u64),
            end_of_stream: self
                .current_frame
                .map(|frame| frame as usize + 1 >= self.frame_count())
                .unwrap_or(false),
        }
    }
}

pub struct OgvPlayback {
    name: String,
    file: OggTheora_File,
    width: u32,
    height: u32,
    frame_duration_ns: Option<f64>,
    start_host_time_ns: u64,
    plane_dims: yuv::PlaneDimensions,
    yuv_buffer: Vec<u8>,
    rgba_buffer: Vec<u8>,
    frame_cursor: Option<u64>,
    end_of_stream: bool,
}

impl OgvPlayback {
    fn new(name: String, path: PathBuf, start_host_time_ns: u64) -> Result<Self> {
        let path_cow = path.to_string_lossy();
        let c_path = std::ffi::CString::new(path_cow.as_bytes())
            .with_context(|| format!("movie path '{}' contains NUL byte", path.display()))?;

        let mut file = MaybeUninit::<OggTheora_File>::zeroed();
        let open_rc = unsafe { tf_fopen(c_path.as_ptr(), file.as_mut_ptr()) };
        if open_rc != 0 {
            return Err(anyhow!(
                "failed to open Theora movie '{}' (error code {open_rc})",
                path.display()
            ));
        }

        let mut file = unsafe { file.assume_init() };

        if unsafe { tf_hasvideo(&mut file) } == 0 {
            unsafe { tf_close(&mut file) };
            return Err(anyhow!(
                "Theora movie '{}' does not contain a video stream",
                path.display()
            ));
        }

        let mut width: i32 = 0;
        let mut height: i32 = 0;
        let mut fps: f64 = 0.0;
        let mut pixel_format: th_pixel_fmt = 0;

        unsafe {
            tf_videoinfo(
                &mut file,
                (&mut width) as *mut i32,
                (&mut height) as *mut i32,
                (&mut fps) as *mut f64,
                (&mut pixel_format) as *mut th_pixel_fmt,
            );
        }

        let width_u32 = match width.try_into() {
            Ok(value) => value,
            Err(_) => {
                unsafe { tf_close(&mut file) };
                return Err(anyhow!("video width overflow for '{}'", path.display()));
            }
        };
        let height_u32 = match height.try_into() {
            Ok(value) => value,
            Err(_) => {
                unsafe { tf_close(&mut file) };
                return Err(anyhow!("video height overflow for '{}'", path.display()));
            }
        };

        let frame_duration_ns = if fps > 0.0 {
            Some(1_000_000_000.0 / fps)
        } else {
            None
        };

        let plane_dims = match yuv::PlaneDimensions::new(
            width_u32 as usize,
            height_u32 as usize,
            pixel_format,
        ) {
            Some(dimensions) => dimensions,
            None => {
                unsafe { tf_close(&mut file) };
                return Err(anyhow!(
                    "unsupported pixel format {pixel_format} for '{}'",
                    path.display()
                ));
            }
        };

        let yuv_len = plane_dims.total_yuv_len().ok_or_else(|| {
            unsafe { tf_close(&mut file) };
            anyhow!("video buffer size overflow for '{}'", path.display())
        })?;

        let rgba_len = plane_dims.rgba_len().ok_or_else(|| {
            unsafe { tf_close(&mut file) };
            anyhow!("RGBA buffer size overflow for '{}'", path.display())
        })?;

        let yuv_buffer = vec![0u8; yuv_len];
        let rgba_buffer = vec![0u8; rgba_len];

        Ok(Self {
            name,
            file,
            width: width_u32,
            height: height_u32,
            frame_duration_ns,
            start_host_time_ns,
            plane_dims,
            yuv_buffer,
            rgba_buffer,
            frame_cursor: None,
            end_of_stream: false,
        })
    }

    fn name(&self) -> &str {
        &self.name
    }

    fn width(&self) -> u32 {
        self.width
    }

    fn height(&self) -> u32 {
        self.height
    }

    fn frame_for_host_time(&mut self, host_time_ns: u64) -> Result<&[u8]> {
        let target_index = if let Some(frame_duration_ns) = self.frame_duration_ns {
            if frame_duration_ns > 0.0 {
                let elapsed = host_time_ns.saturating_sub(self.start_host_time_ns) as f64;
                (elapsed / frame_duration_ns)
                    .floor()
                    .clamp(0.0, u64::MAX as f64) as u64
            } else {
                0
            }
        } else {
            0
        };

        self.ensure_frame(target_index)?;
        Ok(&self.rgba_buffer)
    }

    fn ensure_frame(&mut self, target: u64) -> Result<()> {
        loop {
            match self.frame_cursor {
                Some(current) if current >= target => return Ok(()),
                None | Some(_) => {
                    if self.end_of_stream {
                        return Ok(());
                    }
                    match self.decode_next_frame()? {
                        FrameStatus::Advanced | FrameStatus::Duplicate => continue,
                        FrameStatus::EndOfStream => return Ok(()),
                    }
                }
            }
        }
    }

    fn decode_next_frame(&mut self) -> Result<FrameStatus> {
        let rc = unsafe {
            tf_readvideo(
                &mut self.file,
                self.yuv_buffer.as_mut_ptr() as *mut c_char,
                1,
            )
        };

        match rc {
            1 => {
                self.convert_yuv_to_rgba();
                self.increment_frame_index();
                Ok(FrameStatus::Advanced)
            }
            0 => {
                if unsafe { tf_eos(&mut self.file) } != 0 {
                    self.end_of_stream = true;
                    if self.frame_cursor.is_none() {
                        return Err(anyhow!(
                            "Theora movie '{}' reached end-of-stream without yielding a frame",
                            self.name
                        ));
                    }
                    Ok(FrameStatus::EndOfStream)
                } else {
                    if self.frame_cursor.is_none() {
                        return Err(anyhow!(
                            "Theora movie '{}' produced a duplicate frame before the first frame",
                            self.name
                        ));
                    }
                    self.increment_frame_index();
                    Ok(FrameStatus::Duplicate)
                }
            }
            other => Err(anyhow!(
                "Theora decoder for '{}' returned unexpected status {}",
                self.name,
                other
            )),
        }
    }

    fn increment_frame_index(&mut self) {
        let next = match self.frame_cursor {
            Some(value) => value.saturating_add(1),
            None => 0,
        };
        self.frame_cursor = Some(next);
    }

    fn convert_yuv_to_rgba(&mut self) {
        let (y_plane, u_plane, v_plane) = self.plane_dims.split_planes(&self.yuv_buffer);
        yuv::convert_to_rgba(
            &self.plane_dims,
            y_plane,
            u_plane,
            v_plane,
            &mut self.rgba_buffer,
        );
    }

    fn status(&self) -> PlaybackStatus {
        PlaybackStatus {
            backend: "theora",
            current_frame: self.frame_cursor,
            end_of_stream: self.end_of_stream,
        }
    }
}

impl Drop for OgvPlayback {
    fn drop(&mut self) {
        unsafe {
            tf_close(&mut self.file);
        }
    }
}

enum FrameStatus {
    Advanced,
    Duplicate,
    EndOfStream,
}
