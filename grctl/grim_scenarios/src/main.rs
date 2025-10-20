use std::collections::HashSet;
use std::fs::{self, File};
use std::net::{SocketAddr, TcpListener};
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use clap::{Args, Parser, Subcommand, ValueEnum};
use serde::Serialize;

mod scenario;

use scenario::{LogTailer, ManagedComponent, ScenarioContext};

const INTRO_REQUIRED_MARKERS: [&str; 3] = [
    "manny_office.resume",
    "cut_scene.fullscreen.end intro",
    "actor.mo.tube.interest_actor.complete_chore",
];

#[derive(Parser, Debug)]
#[command(
    name = "grim_scenarios",
    author,
    version,
    about = "Harness for running deterministic Grim Fandango engine scenarios"
)]
struct Cli {
    #[command(subcommand)]
    command: CommandKind,
}

#[derive(Subcommand, Debug)]
enum CommandKind {
    /// Run a predefined scenario and optionally emit artifacts.
    Run(RunArgs),
}

#[derive(Args, Debug)]
struct RunArgs {
    /// Scenario to execute.
    #[arg(value_enum)]
    scenario: ScenarioKind,
    /// Maximum runtime in seconds before the scenario aborts (0 disables).
    #[arg(long, default_value_t = 20.0)]
    timeout: f64,
    /// Directory where scenario artifacts should be written.
    #[arg(long)]
    artifacts_dir: Option<PathBuf>,
    /// Launch grim_viewer alongside the engine for this scenario.
    #[arg(long)]
    with_viewer: bool,
    /// Additional arguments forwarded to grim_viewer (repeat flag, requires --with-viewer).
    #[arg(
        long,
        value_name = "ARG",
        requires = "with_viewer",
        allow_hyphen_values = true
    )]
    viewer_extra: Vec<String>,
    /// Extra hold time in seconds after all markers appear (0 disables).
    #[arg(long, default_value_t = 0.0)]
    hold_seconds: f64,
    /// Launch components and exit immediately without waiting for markers.
    #[arg(long)]
    detach: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
enum ScenarioKind {
    #[value(name = "intro-to-office-computer")]
    IntroToOfficeComputer,
}

impl ScenarioKind {
    fn as_str(self) -> &'static str {
        match self {
            ScenarioKind::IntroToOfficeComputer => "intro-to-office-computer",
        }
    }
}

#[derive(Debug, Serialize)]
struct MarkerObservation {
    marker: String,
    line: String,
    timestamp_ms: u128,
}

#[derive(Debug, Serialize)]
struct ScenarioReport {
    scenario: String,
    elapsed_ms: u128,
    timed_out: bool,
    markers_expected: Vec<String>,
    markers_observed: Vec<MarkerObservation>,
    markers_missing: Vec<String>,
    verification_skipped: bool,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        CommandKind::Run(args) => run_command(args),
    }
}

fn run_command(args: RunArgs) -> Result<()> {
    let timeout = if args.timeout <= 0.0 {
        None
    } else {
        Some(Duration::from_secs_f64(args.timeout))
    };

    let report = match args.scenario {
        ScenarioKind::IntroToOfficeComputer => run_intro_to_office(
            timeout,
            args.with_viewer,
            &args.viewer_extra,
            args.hold_seconds,
            args.detach,
        )?,
    };

    if let Some(dir) = args.artifacts_dir {
        write_report(&dir, &report)?;
    }

    if report.verification_skipped {
        println!(
            "[grim_scenarios] scenario {} launched in detach mode; verification skipped",
            report.scenario
        );
        return Ok(());
    }

    if !report.markers_missing.is_empty() {
        bail!(
            "scenario {} missing markers: {}",
            report.scenario,
            report.markers_missing.join(", ")
        );
    }
    if report.timed_out {
        bail!("scenario {} timed out", report.scenario);
    }

    println!(
        "[grim_scenarios] scenario {} completed in {:.2}s",
        report.scenario,
        report.elapsed_ms as f64 / 1000.0
    );
    Ok(())
}

fn run_intro_to_office(
    timeout: Option<Duration>,
    with_viewer: bool,
    viewer_extra: &[String],
    hold_seconds: f64,
    detach: bool,
) -> Result<ScenarioReport> {
    let ctx = ScenarioContext::new()?;
    ctx.reset_log("grim_engine")?;

    let engine_addr = if with_viewer {
        Some(pick_engine_port()?)
    } else {
        None
    };

    let mut engine_args = vec!["--verbose".to_string()];
    if let Some(addr) = engine_addr {
        engine_args.push("--stream-bind".to_string());
        engine_args.push(addr.to_string());
    } else {
        engine_args.push("--headless".to_string());
    }

    let mut engine = ManagedComponent::start(&ctx, "engine", &engine_args)?;

    let mut viewer = if let Some(addr) = engine_addr {
        let mut args = vec![
            "--engine-stream".to_string(),
            addr.to_string(),
            "--no-retail".to_string(),
        ];
        if !viewer_extra.is_empty() {
            args.push("--".to_string());
            args.extend(viewer_extra.iter().cloned());
        }
        Some(ManagedComponent::start(&ctx, "viewer", &args)?)
    } else {
        None
    };

    let start = Instant::now();
    let deadline = timeout.map(|limit| start + limit);
    let log_path = ctx.log_path("grim_engine");
    let mut tailer = LogTailer::open(&log_path, deadline)?;

    if with_viewer {
        wait_for_viewer_ready(&mut tailer, deadline)?;
    }

    if detach {
        println!(
            "[grim_scenarios] leaving grim_engine and grim_viewer running under grctl supervision"
        );
        println!(
            "[grim_scenarios] stop them later with 'grctl viewer stop' and 'grctl engine stop'"
        );
        if let Some(viewer_comp) = viewer.take() {
            std::mem::forget(viewer_comp);
        }
        std::mem::forget(engine);
        return Ok(ScenarioReport {
            scenario: ScenarioKind::IntroToOfficeComputer.as_str().to_string(),
            elapsed_ms: start.elapsed().as_millis(),
            timed_out: false,
            markers_expected: Vec::new(),
            markers_observed: Vec::new(),
            markers_missing: Vec::new(),
            verification_skipped: true,
        });
    }

    let (observed_markers, seen_markers, timed_out_initial) =
        observe_markers(&mut tailer, start, deadline)?;
    let mut timed_out = timed_out_initial;

    if hold_seconds > 0.0 && !timed_out {
        let hold_duration = Duration::from_secs_f64(hold_seconds);
        timed_out = timed_out || hold_for_duration(&mut tailer, hold_duration, deadline)?;
    }

    if let Some(viewer) = viewer.as_mut() {
        viewer.stop()?;
    }
    engine.stop()?;

    let elapsed_ms = start.elapsed().as_millis();
    let required_markers: Vec<String> = INTRO_REQUIRED_MARKERS
        .iter()
        .map(|marker| marker.to_string())
        .collect();
    let missing_markers: Vec<String> = INTRO_REQUIRED_MARKERS
        .iter()
        .filter(|marker| !seen_markers.contains(*marker))
        .map(|marker| (*marker).to_string())
        .collect();

    Ok(ScenarioReport {
        scenario: ScenarioKind::IntroToOfficeComputer.as_str().to_string(),
        elapsed_ms,
        timed_out,
        markers_expected: required_markers,
        markers_observed: observed_markers,
        markers_missing: missing_markers,
        verification_skipped: false,
    })
}

fn observe_markers(
    tailer: &mut LogTailer,
    start: Instant,
    deadline: Option<Instant>,
) -> Result<(Vec<MarkerObservation>, HashSet<&'static str>, bool)> {
    let mut observed = Vec::new();
    let mut seen: HashSet<&'static str> = HashSet::new();

    loop {
        if seen.len() == INTRO_REQUIRED_MARKERS.len() {
            return Ok((observed, seen, false));
        }

        if let Some(deadline) = deadline {
            if Instant::now() >= deadline {
                return Ok((observed, seen, true));
            }
        }

        match tailer.read_line()? {
            Some(line) => {
                println!("{line}");
                for marker in INTRO_REQUIRED_MARKERS {
                    if !seen.contains(marker) && line.contains(marker) {
                        seen.insert(marker);
                        let observation = MarkerObservation {
                            marker: marker.to_string(),
                            line: line.clone(),
                            timestamp_ms: start.elapsed().as_millis(),
                        };
                        println!(
                            "[grim_scenarios] observed marker: {} at {:.2}s",
                            marker,
                            observation.timestamp_ms as f64 / 1000.0
                        );
                        observed.push(observation);
                    }
                }
            }
            None => thread::sleep(Duration::from_millis(100)),
        }
    }
}

fn hold_for_duration(
    tailer: &mut LogTailer,
    duration: Duration,
    deadline: Option<Instant>,
) -> Result<bool> {
    let mut timed_out = false;
    let mut hold_deadline = Instant::now() + duration;
    if let Some(limit) = deadline {
        hold_deadline = hold_deadline.min(limit);
    }

    while Instant::now() < hold_deadline {
        if let Some(limit) = deadline {
            if Instant::now() >= limit {
                timed_out = true;
                break;
            }
        }

        match tailer.read_line()? {
            Some(line) => println!("{line}"),
            None => thread::sleep(Duration::from_millis(100)),
        }
    }

    Ok(timed_out)
}

fn wait_for_viewer_ready(tailer: &mut LogTailer, deadline: Option<Instant>) -> Result<()> {
    let mut handshake_deadline = Instant::now() + Duration::from_secs(15);
    if let Some(limit) = deadline {
        handshake_deadline = handshake_deadline.min(limit);
    }

    loop {
        if Instant::now() >= handshake_deadline {
            bail!("viewer handshake did not complete before timeout");
        }

        match tailer.read_line()? {
            Some(line) => {
                println!("{line}");
                if line.contains("viewer_ready.open") {
                    println!("[grim_scenarios] viewer handshake complete");
                    return Ok(());
                }
            }
            None => thread::sleep(Duration::from_millis(100)),
        }
    }
}

fn write_report(dir: &Path, report: &ScenarioReport) -> Result<PathBuf> {
    fs::create_dir_all(dir).with_context(|| format!("creating artifact dir {}", dir.display()))?;
    let path = dir.join(format!("{}.json", report.scenario));
    let file = File::create(&path)
        .with_context(|| format!("creating artifact file {}", path.display()))?;
    serde_json::to_writer_pretty(file, report)
        .with_context(|| format!("writing scenario report to {}", path.display()))?;
    println!("[grim_scenarios] wrote report to {}", path.display());
    Ok(path)
}

fn pick_engine_port() -> Result<SocketAddr> {
    let listener = TcpListener::bind(("127.0.0.1", 0)).context("binding ephemeral engine port")?;
    let addr = listener
        .local_addr()
        .context("querying selected engine bind address")?;
    drop(listener);
    Ok(addr)
}
