use anyhow::Result;

mod analysis;
mod assets;
mod audio_bridge;
mod cli;
mod codec3_depth;
mod geometry_diff;
mod geometry_snapshot;
mod lab_collection;
mod lua_host;
mod runtime;
mod scheduler;
mod state;
mod stream;

fn main() -> Result<()> {
    match cli::parse()? {
        cli::Command::RunLua(args) => runtime::execute(args),
        cli::Command::Analyze(args) => analysis::execute(args),
    }
}
