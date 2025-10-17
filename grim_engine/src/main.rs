use anyhow::Result;

mod cli;
mod geometry_snapshot;
mod lab_collection;
mod lua_host;
mod runtime;
mod stream;

fn main() -> Result<()> {
    let args = cli::parse();
    runtime::execute(args)
}
