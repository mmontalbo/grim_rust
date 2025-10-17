use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use std::{fs, io::BufRead};

use anyhow::{Context, Result};
use clap::Parser;
use grim_stream::{
    encode_message, Frame, Hello, MessageKind, PixelFormat, StreamConfig, Telemetry,
    PROTOCOL_VERSION,
};
use thiserror::Error;
use tokio::io::{AsyncReadExt, AsyncWriteExt, BufReader, BufWriter};
use tokio::net::{TcpListener, TcpStream};
use tokio::process::Command;
use tokio::sync::mpsc;

#[derive(Parser, Debug)]
#[command(about = "Retail live capture streamer", version)]
struct Args {
    /// Address the viewer listens on (host:port).
    #[arg(long, default_value = "127.0.0.1:17400")]
    stream_addr: String,

    /// Width of the captured framebuffer.
    #[arg(long, default_value_t = 1280)]
    width: u32,

    /// Height of the captured framebuffer.
    #[arg(long, default_value_t = 720)]
    height: u32,

    /// Expected capture framerate.
    #[arg(long, default_value_t = 30.0)]
    fps: f32,

    /// X11 display to capture from (used when window_id is not provided).
    #[arg(long, default_value = ":0.0")]
    display: String,

    /// Pixel origin offset in the form "X,Y".
    #[arg(long, default_value = "0,0")]
    offset: String,

    /// Optional X11 window ID (hex string). When provided we capture that window only.
    #[arg(long)]
    window_id: Option<String>,

    /// Path to the ffmpeg executable.
    #[arg(long, default_value = "ffmpeg")]
    ffmpeg: String,

    /// Telemetry JSONL file emitted by the retail shim.
    #[arg(long, value_hint = clap::ValueHint::FilePath, default_value = "dev-install/mods/telemetry_events.jsonl")]
    telemetry_events: PathBuf,

    /// Skip forwarding telemetry events.
    #[arg(long)]
    no_telemetry: bool,

    /// Path to a file that should be created when the first frame or telemetry event is forwarded.
    #[arg(long, value_hint = clap::ValueHint::FilePath)]
    ready_notify: Option<PathBuf>,
}

#[derive(Debug, Error)]
enum CaptureError {
    #[error("captured video size must be greater than zero")]
    EmptyFrame,
}

struct ReadyNotifier {
    path: Option<PathBuf>,
    triggered: bool,
}

impl ReadyNotifier {
    fn new(path: Option<PathBuf>) -> Self {
        Self {
            path,
            triggered: false,
        }
    }

    fn mark_ready(&mut self, reason: &str) {
        if self.triggered {
            return;
        }
        let Some(path) = self.path.as_ref() else {
            return;
        };

        let contents = format!("{reason}\n");
        match fs::write(path, contents) {
            Ok(_) => {
                eprintln!(
                    "[live_retail_capture] signalled readiness at {} (reason: {reason})",
                    path.display()
                );
                self.triggered = true;
            }
            Err(err) => {
                eprintln!(
                    "[live_retail_capture] failed to write readiness marker {}: {err:?}",
                    path.display()
                );
            }
        }
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    run(args).await
}

async fn run(args: Args) -> Result<()> {
    if args.width == 0 || args.height == 0 {
        return Err(CaptureError::EmptyFrame.into());
    }
    if args.fps <= 0.0 {
        return Err(anyhow::anyhow!("fps must be positive"));
    }

    let frame_size = (args.width as usize)
        .saturating_mul(args.height as usize)
        .saturating_mul(4);
    if frame_size == 0 {
        return Err(CaptureError::EmptyFrame.into());
    }

    let listener = TcpListener::bind(&args.stream_addr)
        .await
        .with_context(|| format!("binding {}", args.stream_addr))?;
    println!(
        "[live_retail_capture] waiting for viewer at {}",
        args.stream_addr
    );
    let (socket, addr) = listener
        .accept()
        .await
        .with_context(|| format!("accepting viewer connection on {}", args.stream_addr))?;
    println!("[live_retail_capture] viewer connected from {addr}");
    socket.set_nodelay(true)?;
    let mut writer = BufWriter::new(socket);

    send_message(
        &mut writer,
        MessageKind::Hello,
        &Hello::new(
            "retail_capture",
            Some(format!("protocol={:#06x}", PROTOCOL_VERSION)),
        ),
    )
    .await?;

    let config = StreamConfig {
        width: args.width,
        height: args.height,
        pixel_format: PixelFormat::Rgba8,
        stride_bytes: args
            .width
            .checked_mul(4)
            .context("stride calculation overflow")?,
        nominal_fps: Some(args.fps),
    };
    send_message(&mut writer, MessageKind::StreamConfig, &config).await?;

    let mut telemetry_rx = if args.no_telemetry {
        None
    } else {
        match spawn_telemetry_reader(args.telemetry_events.clone()) {
            Ok((rx, _handle)) => Some(rx),
            Err(err) => {
                eprintln!(
                    "[live_retail_capture] telemetry disabled: {err:?} (path: {})",
                    args.telemetry_events.display()
                );
                None
            }
        }
    };

    let mut ready = ReadyNotifier::new(args.ready_notify.clone());

    let mut ffmpeg_child = spawn_ffmpeg(&args, frame_size).await?;
    let stdout = ffmpeg_child
        .stdout
        .take()
        .context("ffmpeg stdout not piped")?;
    let mut reader = BufReader::new(stdout);
    let mut frame_buffer = vec![0u8; frame_size];
    let stream_start = Instant::now();
    let mut frame_id: u64 = 0;

    loop {
        if let Some(rx) = telemetry_rx.as_mut() {
            while let Ok(event) = rx.try_recv() {
                ready.mark_ready("telemetry");
                send_message(&mut writer, MessageKind::Telemetry, &event).await?;
            }
        }

        match reader.read_exact(&mut frame_buffer).await {
            Ok(_) => {
                let host_time_ns = stream_start.elapsed().as_nanos() as u64;
                let frame = Frame {
                    frame_id,
                    host_time_ns,
                    telemetry_time_ns: None,
                    data: frame_buffer.clone(),
                };
                ready.mark_ready("frame");
                send_message(&mut writer, MessageKind::Frame, &frame).await?;
                frame_id = frame_id.wrapping_add(1);
            }
            Err(err) if err.kind() == std::io::ErrorKind::UnexpectedEof => {
                eprintln!("[live_retail_capture] ffmpeg stream ended");
                break;
            }
            Err(err) => return Err(err.into()),
        }
    }

    let status = ffmpeg_child.wait().await?;
    if !status.success() {
        eprintln!("[live_retail_capture] ffmpeg exited with status {status:?}");
    }

    writer.flush().await?;
    Ok(())
}

async fn send_message<T>(
    writer: &mut BufWriter<TcpStream>,
    kind: MessageKind,
    payload: &T,
) -> Result<()>
where
    T: serde::Serialize,
{
    let bytes = encode_message(kind, payload)?;
    writer.write_all(&bytes).await?;
    writer.flush().await?;
    Ok(())
}

async fn spawn_ffmpeg(args: &Args, frame_size: usize) -> Result<tokio::process::Child> {
    let mut command = Command::new(&args.ffmpeg);
    command
        .arg("-hide_banner")
        .arg("-loglevel")
        .arg("warning")
        .arg("-f")
        .arg("x11grab")
        .arg("-video_size")
        .arg(format!("{}x{}", args.width, args.height))
        .arg("-framerate")
        .arg(format!("{}", args.fps));

    if let Some(window_id) = &args.window_id {
        command.arg("-window_id").arg(window_id);
    }

    let mut input = args.display.clone();
    if !args.offset.is_empty() {
        input.push('+');
        input.push_str(&args.offset);
    }
    command.arg("-i").arg(&input);
    command
        .arg("-pix_fmt")
        .arg("rgba")
        .arg("-f")
        .arg("rawvideo")
        .arg("-vsync")
        .arg("0")
        .arg("-");
    command.stdout(std::process::Stdio::piped());
    command.stderr(std::process::Stdio::inherit());

    let child = command
        .spawn()
        .with_context(|| format!("launching ffmpeg (expected frame size {frame_size} bytes)"))?;
    Ok(child)
}

fn spawn_telemetry_reader(
    path: PathBuf,
) -> Result<(
    mpsc::Receiver<Telemetry>,
    tokio::task::JoinHandle<Result<()>>,
)> {
    let (tx, rx) = mpsc::channel(128);
    let handle = tokio::task::spawn_blocking(move || -> Result<()> {
        wait_for_file(path.as_path())?;
        let file = std::fs::File::open(&path)
            .with_context(|| format!("opening telemetry file {}", path.display()))?;
        let mut reader = std::io::BufReader::new(file);
        let mut buffer = String::new();

        loop {
            buffer.clear();
            let bytes = reader.read_line(&mut buffer)?;
            if bytes == 0 {
                std::thread::sleep(Duration::from_millis(50));
                continue;
            }
            let line = buffer.trim();
            if line.is_empty() {
                continue;
            }
            match serde_json::from_str::<Telemetry>(line) {
                Ok(event) => {
                    if tx.blocking_send(event).is_err() {
                        break;
                    }
                }
                Err(err) => {
                    eprintln!(
                        "[live_retail_capture] telemetry parse error ({:?}): {line}",
                        err
                    );
                }
            }
        }
        Ok(())
    });
    Ok((rx, handle))
}

fn wait_for_file(path: &Path) -> Result<()> {
    let mut attempts = 0u32;
    loop {
        if path.exists() {
            return Ok(());
        }
        attempts += 1;
        std::thread::sleep(Duration::from_millis(250));
        if attempts > 240 {
            return Err(anyhow::anyhow!(
                "telemetry file {} not created within timeout",
                path.display()
            ));
        }
    }
}
