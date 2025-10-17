use std::path::PathBuf;

use anyhow::Result;
use clap::Parser;
use grim_formats::SnmFile;

#[derive(Parser)]
struct Args {
    /// Path to an .snm file to inspect.
    input: PathBuf,
}

fn main() -> Result<()> {
    let args = Args::parse();
    let snm = SnmFile::open(&args.input)?;
    println!(
        "SNM {}: {} frames, {}x{}, frame_rate={} flags=0x{:04x}",
        args.input.display(),
        snm.header.frame_count,
        snm.header.width,
        snm.header.height,
        snm.header.frame_rate,
        snm.header.flags
    );
    if let Some(audio) = snm.audio {
        println!(
            "Audio: {} Hz, {} channel(s)",
            audio.sample_rate, audio.channels
        );
    } else {
        println!("Audio: not present");
    }
    println!("Frames parsed: {}", snm.frames.len());
    Ok(())
}
