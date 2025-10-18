use std::path::PathBuf;

use clap::Parser;

#[derive(Parser, Debug)]
#[command(about = "Minimal host for driving the Grim intro sequence", version)]
struct Args {
    /// Path to the extracted DATA000 directory
    #[arg(long, default_value = "extracted/DATA000")]
    data_root: PathBuf,

    /// Run without a viewer and print emitted engine events to stdout
    #[arg(long)]
    headless: bool,

    /// Print additional logging from the Lua host
    #[arg(long)]
    verbose: bool,

    /// Directory containing LAB archives (default: dev-install)
    #[arg(long)]
    lab_root: Option<PathBuf>,

    /// Bind a GrimStream socket and publish real-time state updates
    #[arg(long)]
    stream_bind: Option<String>,

    /// Path to a file that unblocks the live stream loop once the retail capture is ready
    #[arg(long, value_hint = clap::ValueHint::FilePath)]
    stream_ready_file: Option<PathBuf>,
}

#[derive(Debug, Clone)]
pub struct RunLuaArgs {
    pub data_root: PathBuf,
    pub headless: bool,
    pub verbose: bool,
    pub lab_root: Option<PathBuf>,
    pub stream_bind: Option<String>,
    pub stream_ready_file: Option<PathBuf>,
}

pub fn parse() -> RunLuaArgs {
    let args = Args::parse();
    RunLuaArgs {
        data_root: args.data_root,
        headless: args.headless,
        verbose: args.verbose,
        lab_root: args.lab_root,
        stream_bind: args.stream_bind,
        stream_ready_file: args.stream_ready_file,
    }
}
