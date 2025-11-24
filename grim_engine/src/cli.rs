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

}

#[derive(Debug, Clone)]
pub struct RunLuaArgs {
    pub data_root: PathBuf,
    pub headless: bool,
    pub verbose: bool,
}

pub fn parse() -> RunLuaArgs {
    let args = Args::parse();
    RunLuaArgs {
        data_root: args.data_root,
        headless: args.headless,
        verbose: args.verbose,
    }
}
