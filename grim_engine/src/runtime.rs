use anyhow::Result;

use crate::cli::RunLuaArgs;
use crate::lua_host::run_boot_sequence;
use crate::stream::StreamServer;

pub fn execute(args: RunLuaArgs) -> Result<()> {
    let RunLuaArgs {
        data_root,
        headless,
        verbose,
        lab_root,
        stream_bind,
        stream_ready_file,
    } = args;

    let stream = if headless {
        if let Some(addr) = stream_bind.as_ref() {
            eprintln!(
                "[grim_engine] warning: ignoring --stream-bind {} in headless mode",
                addr
            );
        }
        None
    } else if let Some(addr) = stream_bind.as_ref() {
        Some(StreamServer::bind(
            addr,
            Some(env!("CARGO_PKG_VERSION").to_string()),
        )?)
    } else {
        None
    };

    let runtime = run_boot_sequence(
        &data_root,
        lab_root.as_deref(),
        verbose,
        headless,
        None,
        None,
        stream,
        stream_ready_file,
    )?;

    if let Some(runtime) = runtime {
        runtime.run()?;
    }

    Ok(())
}
