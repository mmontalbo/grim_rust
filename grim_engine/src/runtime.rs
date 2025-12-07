use anyhow::Result;

use crate::cli::RunLuaArgs;
use crate::lua_host::{log_engine_exit, run_boot_sequence};

pub fn execute(args: RunLuaArgs) -> Result<()> {
    let RunLuaArgs {
        data_root,
        headless,
        verbose,
    } = args;

    let result = (|| -> Result<()> {
        let runtime = run_boot_sequence(&data_root, verbose, headless)?;
        runtime.run()?;
        Ok(())
    })();

    match result {
        Ok(()) => {
            log_engine_exit("ok", None);
            Ok(())
        }
        Err(err) => {
            log_engine_exit("error", Some(&err.to_string()));
            Err(err)
        }
    }
}
