use std::fs::File;
use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::Parser;
use grim_formats::{BmFile, decode_bm, decode_bm_with_seed};
use png::{BitDepth, ColorType, Encoder};

#[derive(Parser)]
#[command(
    author,
    version,
    about = "Decode Grim Fandango BM/ZBM files into RGBA PNGs."
)]
struct Args {
    /// Path to the input .bm or .zbm file.
    input: PathBuf,
    /// Output directory where PNG files will be written.
    output: PathBuf,
    /// Optional seed bitmap (typically the matching .bm when decoding a .zbm).
    #[arg(long)]
    seed: Option<PathBuf>,
    /// Skip frames that already exist on disk.
    #[arg(long)]
    skip_existing: bool,
}

fn main() -> Result<()> {
    let args = Args::parse();
    std::fs::create_dir_all(&args.output)
        .with_context(|| format!("failed to create {}", args.output.display()))?;

    let input_bytes =
        std::fs::read(&args.input).with_context(|| format!("reading {}", args.input.display()))?;
    let seed_bytes = match args.seed {
        Some(ref seed_path) => Some(
            std::fs::read(seed_path)
                .with_context(|| format!("reading seed {}", seed_path.display()))?,
        ),
        None => None,
    };

    let bm: BmFile = match seed_bytes {
        Some(ref seed) => decode_bm_with_seed(&input_bytes, Some(seed))
            .with_context(|| format!("decoding {}", args.input.display()))?,
        None => {
            decode_bm(&input_bytes).with_context(|| format!("decoding {}", args.input.display()))?
        }
    };
    let metadata = bm.metadata();

    let stem = args
        .input
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("bm_frame");

    for (index, frame) in bm.frames.iter().enumerate() {
        let output_path =
            args.output
                .join(format!("{stem}_{index:02}.png", stem = stem, index = index));
        if args.skip_existing && output_path.exists() {
            continue;
        }

        let rgba = frame
            .as_rgba8888(&metadata)
            .with_context(|| format!("converting frame {index} to RGBA for {}", stem))?;

        write_rgba_png(&output_path, frame.width, frame.height, &rgba)
            .with_context(|| format!("writing {}", output_path.display()))?;
    }

    println!(
        "Exported {} frame(s) from {} into {}",
        bm.frames.len(),
        args.input.display(),
        args.output.display()
    );

    Ok(())
}

fn write_rgba_png(path: &PathBuf, width: u32, height: u32, pixels: &[u8]) -> Result<()> {
    let file = File::create(path).with_context(|| format!("creating {}", path.display()))?;
    let mut encoder = Encoder::new(file, width, height);
    encoder.set_color(ColorType::Rgba);
    encoder.set_depth(BitDepth::Eight);
    let mut writer = encoder
        .write_header()
        .with_context(|| format!("writing PNG header {}", path.display()))?;
    writer
        .write_image_data(pixels)
        .with_context(|| format!("writing PNG pixels {}", path.display()))?;
    Ok(())
}
