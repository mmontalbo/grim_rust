use anyhow::Result;
use clap::Parser;
use uuid::Uuid;

mod cli;
mod components;
mod gdb;
mod lifecycle;
mod log_follow;
mod parity;
mod retail;
mod stack_dump;
mod trace_tui;

use cli::{
    Cli, CommandKind, EngineCommand, EngineStart, ParityCommand, ParityStartArgs, ParityStopArgs,
    RetailCommand, RetailCopy, RetailStart,
};
use components::{print_component_status, ComponentKind, Paths};
use lifecycle::{start_engine, start_retail, stop_component};
use log_follow::show_logs;
use parity::parity_logs;
use retail::{RetailLayout, SymbolMapStatus};

fn main() -> Result<()> {
    let cli = Cli::parse();
    let paths = Paths::discover()?;
    let _ = stack_dump::StackDumpRecord::schema_version();

    match cli.command {
        CommandKind::Engine(cmd) => handle_engine(cmd, &paths),
        CommandKind::Retail(cmd) => handle_retail(cmd, &paths),
        CommandKind::Parity(cmd) => handle_parity(cmd, &paths),
        CommandKind::Status => {
            for component in [ComponentKind::Engine, ComponentKind::Retail] {
                print_component_status(&paths, component)?;
            }
            Ok(())
        }
    }
}

fn handle_engine(cmd: EngineCommand, paths: &Paths) -> Result<()> {
    match cmd {
        EngineCommand::Start(args) => start_engine(args, paths).map(|_| ()),
        EngineCommand::Stop => stop_component(ComponentKind::Engine, paths, false),
        EngineCommand::Status => {
            print_component_status(paths, ComponentKind::Engine)?;
            Ok(())
        }
        EngineCommand::Logs(args) => show_logs(paths, ComponentKind::Engine, &args),
    }
}

fn handle_retail(cmd: RetailCommand, paths: &Paths) -> Result<()> {
    match cmd {
        RetailCommand::Start(args) => start_retail(args, paths).map(|_| ()),
        RetailCommand::Stop => stop_component(ComponentKind::Retail, paths, true),
        RetailCommand::Status => {
            print_component_status(paths, ComponentKind::Retail)?;
            print_retail_instrumentation(paths)?;
            Ok(())
        }
        RetailCommand::Logs(args) => show_logs(paths, ComponentKind::Retail, &args),
        RetailCommand::Copy(args) => copy_retail(args, paths),
    }
}

fn handle_parity(cmd: ParityCommand, paths: &Paths) -> Result<()> {
    match cmd {
        ParityCommand::Start(args) => parity_start(args, paths),
        ParityCommand::Logs(args) => parity_logs(args, paths),
        ParityCommand::Stop(args) => parity_stop(args, paths),
        ParityCommand::Status => {
            print_component_status(paths, ComponentKind::Engine)?;
            print_component_status(paths, ComponentKind::Retail)?;
            Ok(())
        }
    }
}

fn parity_start(args: ParityStartArgs, paths: &Paths) -> Result<()> {
    let run_id = args
        .run_id
        .clone()
        .unwrap_or_else(|| Uuid::new_v4().to_string());

    let mut guard = LaunchGuard::default();

    let engine_args = EngineStart {
        release: args.engine_release,
        headless: args.engine_headless,
        verbose: false,
        attach: false,
        run_id: Some(run_id.clone()),
        extra_args: Vec::new(),
    };
    let engine_launch = start_engine(engine_args, paths)?;
    guard.push(ComponentKind::Engine);

    let retail_args = RetailStart {
        timeout: if args.no_timeout {
            "0".to_string()
        } else {
            args.timeout.clone()
        },
        no_timeout: args.no_timeout,
        vanilla: args.retail_vanilla,
        attach: false,
        debugger: None,
        run_id: Some(run_id.clone()),
        extra_args: Vec::new(),
    };
    let retail_launch = match start_retail(retail_args, paths) {
        Ok(info) => info,
        Err(err) => {
            guard.stop_all(paths);
            return Err(err);
        }
    };

    println!("[grctl] parity run {}", engine_launch.run_id);
    println!("  engine log:   {}", engine_launch.log_path.display());
    println!("  retail log:   {}", retail_launch.log_path.display());
    println!(
        "  telemetry:    {}",
        paths.retail_telemetry_path().display()
    );
    println!(
        "Next: grctl engine logs --run {} -f | grctl retail logs --run {} -f",
        engine_launch.run_id, retail_launch.run_id
    );

    Ok(())
}

fn parity_stop(args: ParityStopArgs, paths: &Paths) -> Result<()> {
    let mut last_err: Option<anyhow::Error> = None;
    for component in [ComponentKind::Engine, ComponentKind::Retail] {
        let force = args.force || matches!(component, ComponentKind::Retail);
        if let Err(err) = stop_component(component, paths, force) {
            eprintln!(
                "[grctl] warning: failed to stop {}: {err:?}",
                component.display()
            );
            last_err = Some(err);
        }
    }
    if let Some(err) = last_err {
        Err(err)
    } else {
        Ok(())
    }
}

fn copy_retail(args: RetailCopy, paths: &Paths) -> Result<()> {
    let layout = RetailLayout::new(&paths.repo_root)?;
    let destination = layout.sync_from(args.source.as_deref(), args.force)?;
    println!("[grctl] copied retail install to {}", destination.display());
    Ok(())
}

fn print_retail_instrumentation(paths: &Paths) -> Result<()> {
    let layout = RetailLayout::new(&paths.repo_root)?;
    if !layout.dev_install().exists() {
        println!(
            "[grctl] {:<12} instrumentation: dev-install missing (run 'grctl retail copy')",
            ComponentKind::Retail.as_str()
        );
        return Ok(());
    }
    let status = layout.instrumentation_status()?;
    println!(
        "[grctl] {:<12} instrumentation: {}",
        ComponentKind::Retail.as_str(),
        describe_instrumentation(&status)
    );
    Ok(())
}

fn describe_instrumentation(status: &retail::InstrumentationStatus) -> String {
    if !status.shim_available {
        return "vanilla (shim missing; build grim_analysis)".to_string();
    }
    if status.symbol_map == SymbolMapStatus::Fresh
        && status.liblua_symbol_map == SymbolMapStatus::Fresh
    {
        return "instrumented (shim + symbol maps ready)".to_string();
    }
    format!(
        "instrumented (shim ready, symbol maps: retail={}, libLua={})",
        describe_map_status(status.symbol_map),
        describe_map_status(status.liblua_symbol_map),
    )
}

fn describe_map_status(status: SymbolMapStatus) -> &'static str {
    match status {
        SymbolMapStatus::Fresh => "fresh",
        SymbolMapStatus::Stale => "stale",
        SymbolMapStatus::Missing => "missing",
    }
}

#[derive(Default)]
struct LaunchGuard {
    components: Vec<ComponentKind>,
}

impl LaunchGuard {
    fn push(&mut self, component: ComponentKind) {
        self.components.push(component);
    }

    fn stop_all(&mut self, paths: &Paths) {
        for component in self.components.iter().rev() {
            if let Err(err) = stop_component(*component, paths, false) {
                eprintln!(
                    "[grctl] warning: failed to stop {}: {err:?}",
                    component.display()
                );
            }
        }
        self.components.clear();
    }
}
