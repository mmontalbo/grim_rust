use std::collections::HashSet;
use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use clap::{Args, Parser, Subcommand, ValueEnum};
use serde::Serialize;

mod scenario;
mod timeline;

use scenario::{LogTailer, ManagedComponent, ScenarioContext};
use timeline::IntroTimelineReport;

const INTRO_TIMELINE_MARKERS: [&str; 4] = [
    "movie.logos.start",
    "movie.logos.end",
    "movie.intro.start",
    "movie.intro.end",
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
    #[value(name = "intro-to-office-tube")]
    IntroToOfficeTube,
}

impl ScenarioKind {
    fn as_str(self) -> &'static str {
        match self {
            ScenarioKind::IntroToOfficeComputer => "intro-to-office-computer",
            ScenarioKind::IntroToOfficeTube => "intro-to-office-tube",
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
    intro_timeline: Option<IntroTimelineReport>,
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

    let report = run_intro_to_office(
        args.scenario,
        &INTRO_TIMELINE_MARKERS,
        timeout,
        args.hold_seconds,
        args.detach,
    )?;

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
    kind: ScenarioKind,
    required_markers: &[&'static str],
    timeout: Option<Duration>,
    hold_seconds: f64,
    detach: bool,
) -> Result<ScenarioReport> {
    let ctx = ScenarioContext::new()?;
    ctx.reset_log("grim_engine")?;

    let engine_args = vec!["--verbose".to_string(), "--headless".to_string()];
    let mut engine = ManagedComponent::start(&ctx, "engine", &engine_args)?;

    let start = Instant::now();
    let deadline = timeout.map(|limit| start + limit);
    let log_path = ctx.log_path("grim_engine");
    let mut tailer = LogTailer::open(&log_path, deadline)?;

    if detach {
        println!("[grim_scenarios] leaving grim_engine running under grctl supervision");
        std::mem::forget(engine);
        return Ok(ScenarioReport {
            scenario: kind.as_str().to_string(),
            elapsed_ms: start.elapsed().as_millis(),
            timed_out: false,
            markers_expected: Vec::new(),
            markers_observed: Vec::new(),
            markers_missing: Vec::new(),
            verification_skipped: true,
            intro_timeline: None,
        });
    }

    let (observed_markers, seen_markers, timed_out_initial) =
        observe_markers(required_markers, &mut tailer, start, deadline)?;
    let mut timed_out = timed_out_initial;

    if hold_seconds > 0.0 && !timed_out {
        let hold_duration = Duration::from_secs_f64(hold_seconds);
        timed_out = timed_out || hold_for_duration(&mut tailer, hold_duration, deadline)?;
    }

    engine.stop()?;

    let elapsed_ms = start.elapsed().as_millis();
    let expected: Vec<String> = required_markers
        .iter()
        .map(|marker| (*marker).to_string())
        .collect();
    let missing: Vec<String> = required_markers
        .iter()
        .filter(|marker| !seen_markers.contains(*marker))
        .map(|marker| (*marker).to_string())
        .collect();

    Ok(ScenarioReport {
        scenario: kind.as_str().to_string(),
        elapsed_ms,
        timed_out,
        markers_expected: expected,
        markers_observed: observed_markers,
        markers_missing: missing,
        verification_skipped: false,
        intro_timeline: None,
    })
}

fn observe_markers(
    required_markers: &[&'static str],
    tailer: &mut LogTailer,
    start: Instant,
    deadline: Option<Instant>,
) -> Result<(Vec<MarkerObservation>, HashSet<&'static str>, bool)> {
    let mut observed = Vec::new();
    let mut seen: HashSet<&'static str> = HashSet::new();

    loop {
        if seen.len() == required_markers.len() {
            return Ok((observed, seen, false));
        }

        if let Some(deadline) = deadline && Instant::now() >= deadline {
            return Ok((observed, seen, true));
        }

        match tailer.read_line()? {
            Some(line) => {
                println!("{line}");
                for marker in required_markers {
                    if !seen.contains(marker) && line.contains(marker) {
                        seen.insert(*marker);
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
        if let Some(limit) = deadline && Instant::now() >= limit {
            timed_out = true;
            break;
        }

        match tailer.read_line()? {
            Some(line) => println!("{line}"),
            None => thread::sleep(Duration::from_millis(100)),
        }
    }

    Ok(timed_out)
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
