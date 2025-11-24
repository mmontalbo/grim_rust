use anyhow::Result;

use crate::cli::RunLuaArgs;
use crate::lua_host::run_boot_sequence;

pub fn execute(args: RunLuaArgs) -> Result<()> {
    let RunLuaArgs {
        data_root,
        headless,
        verbose,
    } = args;

    let runtime = run_boot_sequence(&data_root, verbose, headless)?;
    runtime.run()?;

    Ok(())
}
