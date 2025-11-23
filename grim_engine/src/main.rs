use anyhow::Result;

mod cli;
mod lab_collection;
mod lua_host;
mod runtime;

fn main() -> Result<()> {
    let args = cli::parse();
    runtime::execute(args)
}
