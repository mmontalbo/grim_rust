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
mod timeline;

use scenario::{LogTailer, ManagedComponent, ScenarioContext};
use timeline::{
    IntroTimelineReport, analyze_intro_timeline, parse_intro_timeline_event,
    summarize_intro_timeline,
};

const INTRO_COMPUTER_REQUIRED_MARKERS: [&str; 3] = [
    "manny_office.resume",
    "cut_scene.fullscreen.end intro",
    "actor.mo.tube.interest_actor.complete_chore",
];

const INTRO_TUBE_REQUIRED_MARKERS: [&str; 4] = [
    "manny_office.resume",
    "cut_scene.fullscreen.end intro",
    "actor.motx083tube.complete_chore mo_tube_set_closed_w_can",
    "actor.mo.tube.interest_actor.complete_chore mo_tube_set_closed_w_can",
];

const INTRO_RETAIL_REQUIRED_EVENTS: [&str; 4] = [
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
    /// Launch the retail capture so grim_viewer shows the retail pane.
    #[arg(long)]
    with_retail: bool,
    /// Skip the Rust engine entirely and only launch the retail capture build.
    #[arg(long)]
    retail_only: bool,
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

    let report = match args.scenario {
        ScenarioKind::IntroToOfficeComputer => run_intro_to_office(
            ScenarioKind::IntroToOfficeComputer,
            &INTRO_COMPUTER_REQUIRED_MARKERS,
            timeout,
            args.with_viewer,
            &args.viewer_extra,
            args.with_retail || args.retail_only,
            args.hold_seconds,
            args.detach,
            args.retail_only,
        )?,
        ScenarioKind::IntroToOfficeTube => run_intro_to_office(
            ScenarioKind::IntroToOfficeTube,
            &INTRO_TUBE_REQUIRED_MARKERS,
            timeout,
            args.with_viewer,
            &args.viewer_extra,
            args.with_retail || args.retail_only,
            args.hold_seconds,
            args.detach,
            args.retail_only,
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
    kind: ScenarioKind,
    required_markers: &[&'static str],
    timeout: Option<Duration>,
    with_viewer: bool,
    viewer_extra: &[String],
    with_retail: bool,
    hold_seconds: f64,
    detach: bool,
    retail_only: bool,
) -> Result<ScenarioReport> {
    if retail_only && with_viewer {
        bail!("retail-only mode cannot launch the viewer");
    }
    if with_retail && !with_viewer && !retail_only {
        bail!("retail capture requires the viewer to be running (or use --retail-only)");
    }
    let ctx = ScenarioContext::new()?;
    if !retail_only {
        ctx.reset_log("grim_engine")?;
    }
    if with_retail || retail_only {
        ctx.reset_retail_telemetry()?;
    }

    if retail_only {
        return run_retail_intro_only(kind, timeout, hold_seconds, detach, &ctx);
    }

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
        let mut args = vec!["--engine-stream".to_string(), addr.to_string()];
        if !with_retail {
            args.push("--no-retail".to_string());
        }
        if !viewer_extra.is_empty() {
            args.push("--".to_string());
            args.extend(viewer_extra.iter().cloned());
        }
        Some(ManagedComponent::start(&ctx, "viewer", &args)?)
    } else {
        None
    };
    let mut retail = None;

    let start = Instant::now();
    let deadline = timeout.map(|limit| start + limit);
    let log_path = ctx.log_path("grim_engine");
    let mut tailer = LogTailer::open(&log_path, deadline)?;

    if with_viewer {
        wait_for_viewer_ready(&mut tailer, deadline)?;
        if with_retail {
            let retail_args = vec!["--no-timeout".to_string()];
            retail = Some(ManagedComponent::start(&ctx, "retail", &retail_args)?);
        }
    }

    if detach {
        println!(
            "[grim_scenarios] leaving grim_engine and grim_viewer running under grctl supervision"
        );
        if with_retail {
            println!("[grim_scenarios] retail capture left running under grctl supervision");
            println!(
                "[grim_scenarios] stop components later with 'grctl retail stop', 'grctl viewer stop', and 'grctl engine stop'"
            );
        } else {
            println!(
                "[grim_scenarios] stop them later with 'grctl viewer stop' and 'grctl engine stop'"
            );
        }
        if let Some(viewer_comp) = viewer.take() {
            std::mem::forget(viewer_comp);
        }
        if let Some(retail_comp) = retail.take() {
            std::mem::forget(retail_comp);
        }
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

    if let Some(viewer) = viewer.as_mut() {
        viewer.stop()?;
    }
    if let Some(retail) = retail.as_mut() {
        retail.stop()?;
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
    let intro_timeline = if with_retail && !retail_only {
        match analyze_intro_timeline(&ctx, &log_path) {
            Ok(report) => report,
            Err(err) => {
                eprintln!("[grim_scenarios] warning: intro timeline comparison skipped: {err:?}");
                None
            }
        }
    } else {
        None
    };
    if let Some(timeline) = intro_timeline.as_ref() {
        summarize_intro_timeline(timeline);
    } else if with_retail {
        eprintln!(
            "[grim_scenarios] warning: intro timeline comparison unavailable; see earlier logs for details"
        );
    }

    Ok(ScenarioReport {
        scenario: kind.as_str().to_string(),
        elapsed_ms,
        timed_out,
        markers_expected: expected,
        markers_observed: observed_markers,
        markers_missing: missing,
        verification_skipped: false,
        intro_timeline,
    })
}

fn run_retail_intro_only(
    kind: ScenarioKind,
    timeout: Option<Duration>,
    hold_seconds: f64,
    detach: bool,
    ctx: &ScenarioContext,
) -> Result<ScenarioReport> {
    println!(
        "[grim_scenarios] running {} in retail-only mode",
        kind.as_str()
    );
    let start = Instant::now();
    let deadline = timeout.map(|limit| start + limit);
    let telemetry_path = ctx.telemetry_events_path();
    let mut tailer = LogTailer::open(&telemetry_path, deadline)?;
    let mut retail = ManagedComponent::start(ctx, "retail", &["--no-timeout".to_string()])?;

    if detach {
        println!("[grim_scenarios] leaving retail capture running under grctl supervision");
        println!("[grim_scenarios] stop it later with 'grctl retail stop'");
        std::mem::forget(retail);
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

    let (observed_events, seen_events, mut timed_out) =
        observe_retail_intro_events(&INTRO_RETAIL_REQUIRED_EVENTS, &mut tailer, start, deadline)?;
    if hold_seconds > 0.0 && !timed_out {
        let hold_duration = Duration::from_secs_f64(hold_seconds);
        timed_out = hold_for_duration(&mut tailer, hold_duration, deadline)?;
    }

    retail.stop()?;

    let elapsed_ms = start.elapsed().as_millis();
    let expected: Vec<String> = INTRO_RETAIL_REQUIRED_EVENTS
        .iter()
        .map(|event| (*event).to_string())
        .collect();
    let missing: Vec<String> = INTRO_RETAIL_REQUIRED_EVENTS
        .iter()
        .filter(|event| !seen_events.contains(*event))
        .map(|event| (*event).to_string())
        .collect();

    Ok(ScenarioReport {
        scenario: kind.as_str().to_string(),
        elapsed_ms,
        timed_out,
        markers_expected: expected,
        markers_observed: observed_events,
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

        if let Some(deadline) = deadline {
            if Instant::now() >= deadline {
                return Ok((observed, seen, true));
            }
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

fn observe_retail_intro_events(
    required_events: &[&'static str],
    tailer: &mut LogTailer,
    start: Instant,
    deadline: Option<Instant>,
) -> Result<(Vec<MarkerObservation>, HashSet<&'static str>, bool)> {
    let mut observed = Vec::new();
    let mut seen: HashSet<&'static str> = HashSet::new();

    loop {
        if seen.len() == required_events.len() {
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
                if let Some(event) = parse_intro_timeline_event(&line) {
                    for required in required_events {
                        if !seen.contains(required) && event == *required {
                            seen.insert(*required);
                            let observation = MarkerObservation {
                                marker: required.to_string(),
                                line: line.clone(),
                                timestamp_ms: start.elapsed().as_millis(),
                            };
                            println!(
                                "[grim_scenarios] observed retail intro event: {} at {:.2}s",
                                required,
                                observation.timestamp_ms as f64 / 1000.0
                            );
                            observed.push(observation);
                        }
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
    const VIEWER_HANDSHAKE_TIMEOUT_SECS: u64 = 60;
    let mut handshake_deadline =
        Instant::now() + Duration::from_secs(VIEWER_HANDSHAKE_TIMEOUT_SECS);
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
