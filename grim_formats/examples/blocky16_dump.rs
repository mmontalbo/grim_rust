use std::fs::File;
use std::io::Write;
use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::{Parser, ValueEnum};
use grim_formats::SnmFile;

#[derive(Parser)]
#[command(
    author,
    version,
    about = "Decode Blocky16 frames from an SNM into raw dumps."
)]
struct Args {
    /// Path to the input .snm file.
    input: PathBuf,
    /// Output directory where decoded frames will be written.
    output: PathBuf,
    /// Output pixel format for decoded frames (default: rgba).
    #[arg(long, value_enum, default_value_t = OutputFormat::Rgba)]
    format: OutputFormat,
    /// Optional limit on the number of frames to decode.
    #[arg(long)]
    limit: Option<usize>,
    /// Skip overwriting frames that already exist on disk.
    #[arg(long)]
    skip_existing: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
enum OutputFormat {
    Rgba,
    #[value(name = "1555", alias = "packed1555")]
    Packed1555,
}

impl OutputFormat {
    fn extension(self) -> &'static str {
        match self {
            OutputFormat::Rgba => "rgba",
            OutputFormat::Packed1555 => "1555",
        }
    }

    fn buffer_len(self, snm: &SnmFile) -> usize {
        match self {
            OutputFormat::Rgba => snm.blocky16_rgba_len(),
            OutputFormat::Packed1555 => snm.blocky16_frame_len(),
        }
    }
}

fn main() -> Result<()> {
    let args = Args::parse();
    std::fs::create_dir_all(&args.output)
        .with_context(|| format!("failed to create {}", args.output.display()))?;

    let snm = SnmFile::open(&args.input)?;
    let mut decoder = snm
        .blocky16_decoder()
        .with_context(|| format!("failed to create decoder for {}", args.input.display()))?;
    let mut scratch = vec![0u8; args.format.buffer_len(&snm)];

    let mut written = 0usize;
    for frame in &snm.frames {
        if let Some(limit) = args.limit {
            if written >= limit {
                break;
            }
        }

        let decoded = match args.format {
            OutputFormat::Rgba => frame.decode_blocky16_rgba(&mut decoder, &mut scratch)?,
            OutputFormat::Packed1555 => frame.decode_blocky16(&mut decoder, &mut scratch)?,
        };
        if !decoded {
            continue;
        }

        let filename = format!("frame_{:05}.{}", frame.index, args.format.extension());
        let output_path = args.output.join(filename);
        if args.skip_existing && output_path.exists() {
            continue;
        }

        // Dump out the raw pixel buffer for offline parity checks.
        let mut file = File::create(&output_path)
            .with_context(|| format!("failed to create {}", output_path.display()))?;
        file.write_all(&scratch)
            .with_context(|| format!("failed to write {}", output_path.display()))?;
        written += 1;
    }

    println!(
        "Decoded {written} frame(s) from {} into {} ({:?} pixels)",
        args.input.display(),
        args.output.display(),
        args.format
    );

    Ok(())
}
