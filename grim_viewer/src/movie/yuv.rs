use theorafile_rs::{
    th_pixel_fmt, th_pixel_fmt_TH_PF_420, th_pixel_fmt_TH_PF_422, th_pixel_fmt_TH_PF_444,
};

#[derive(Debug, Clone, Copy)]
pub struct PlaneDimensions {
    width: usize,
    height: usize,
    uv_width: usize,
    uv_height: usize,
    pixel_format: th_pixel_fmt,
}

impl PlaneDimensions {
    pub fn new(width: usize, height: usize, pixel_format: th_pixel_fmt) -> Option<Self> {
        let (uv_width, uv_height) = match pixel_format {
            pf if pf == th_pixel_fmt_TH_PF_420 => ((width / 2).max(1), (height / 2).max(1)),
            pf if pf == th_pixel_fmt_TH_PF_422 => ((width / 2).max(1), height),
            pf if pf == th_pixel_fmt_TH_PF_444 => (width, height),
            _ => return None,
        };
        Some(Self {
            width,
            height,
            uv_width,
            uv_height,
            pixel_format,
        })
    }

    pub fn total_yuv_len(&self) -> Option<usize> {
        let y_plane = self.width.checked_mul(self.height)?;
        let uv_plane = self.uv_width.checked_mul(self.uv_height)?;
        let chroma = uv_plane.checked_mul(2)?;
        y_plane.checked_add(chroma)
    }

    pub fn rgba_len(&self) -> Option<usize> {
        self.width.checked_mul(self.height)?.checked_mul(4)
    }

    pub fn split_planes<'a>(&self, buffer: &'a [u8]) -> (&'a [u8], &'a [u8], &'a [u8]) {
        let y_plane_len = self.width * self.height;
        let uv_plane_len = self.uv_width * self.uv_height;
        let y_plane = &buffer[..y_plane_len];
        let u_start = y_plane_len;
        let v_start = u_start + uv_plane_len;
        let u_plane = &buffer[u_start..v_start];
        let v_plane = &buffer[v_start..v_start + uv_plane_len];
        (y_plane, u_plane, v_plane)
    }

    pub fn pixel_format(&self) -> th_pixel_fmt {
        self.pixel_format
    }

    pub fn pixel_format_label(&self) -> &'static str {
        match self.pixel_format {
            pf if pf == th_pixel_fmt_TH_PF_420 => "4:2:0",
            pf if pf == th_pixel_fmt_TH_PF_422 => "4:2:2",
            pf if pf == th_pixel_fmt_TH_PF_444 => "4:4:4",
            _ => "unknown",
        }
    }

    pub fn width(&self) -> usize {
        self.width
    }

    pub fn height(&self) -> usize {
        self.height
    }

    pub fn chroma_width(&self) -> usize {
        self.uv_width
    }

    pub fn chroma_height(&self) -> usize {
        self.uv_height
    }
}

pub fn convert_to_rgba(
    dims: &PlaneDimensions,
    y_plane: &[u8],
    u_plane: &[u8],
    v_plane: &[u8],
    output: &mut [u8],
) {
    let width = dims.width();
    let height = dims.height();
    for y in 0..height {
        for x in 0..width {
            let y_sample = y_plane[y * width + x] as f32;
            let u_sample = sample_chroma(u_plane, x, y, dims);
            let v_sample = sample_chroma(v_plane, x, y, dims);
            let (r, g, b) = ycbcr_to_rgb(y_sample, u_sample, v_sample);
            let idx = (y * width + x) * 4;
            output[idx] = r;
            output[idx + 1] = g;
            output[idx + 2] = b;
            output[idx + 3] = 255;
        }
    }
}

fn sample_chroma(plane: &[u8], x: usize, y: usize, dims: &PlaneDimensions) -> f32 {
    let plane_width = dims.chroma_width();
    let plane_height = dims.chroma_height();
    let pixel_format = dims.pixel_format();

    let sample_x = match pixel_format {
        pf if pf == th_pixel_fmt_TH_PF_420 || pf == th_pixel_fmt_TH_PF_422 => x / 2,
        _ => x,
    }
    .min(plane_width.saturating_sub(1));

    let sample_y = match pixel_format {
        pf if pf == th_pixel_fmt_TH_PF_420 => y / 2,
        _ => y,
    }
    .min(plane_height.saturating_sub(1));

    plane[sample_y * plane_width + sample_x] as f32
}

fn ycbcr_to_rgb(y: f32, cb: f32, cr: f32) -> (u8, u8, u8) {
    let y = y;
    let cb = cb - 128.0;
    let cr = cr - 128.0;

    let r = (y + 1.402_f32 * cr).clamp(0.0, 255.0);
    let g = (y - 0.344136_f32 * cb - 0.714136_f32 * cr).clamp(0.0, 255.0);
    let b = (y + 1.772_f32 * cb).clamp(0.0, 255.0);

    (r as u8, g as u8, b as u8)
}
