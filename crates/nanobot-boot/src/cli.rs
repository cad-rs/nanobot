//! Product command-line entry helpers (keeps downstream product `main`s tiny).
//!
//! Parses the common modes (flags are recognized anywhere in `argv`, since
//! products add their own):
//! - (default) run the GUI;
//! - `--selfcheck`: headless selfcheck, process exit code 0/1;
//! - `--selfcheck-crash`: crash-recovery probe (invoked by the selfcheck as a
//!   subprocess, three times: `NB_CRASH_PHASE=A` crashes without running
//!   `Drop`, `B` recovers and verifies, `C` verifies the clean marker
//!   suppresses recovery; requires the `NB_CRASH_DIR` environment variable);
//! - `--agent-smoke`: assemble the harness, drive one turn headlessly, report;
//! - `--agent-run <session-id>`: assemble for the given session, drive one
//!   turn, report the event count.
//!
//! A typical product `main`:
//!
//! ```text
//! fn main() {
//!     let cli = nanobot_boot::cli::Cli::from_env();
//!     let make_builder = || product_assembly();
//!     match cli.mode {
//!         CliMode::Selfcheck => nanobot_boot::selfcheck::exit_with(&make_builder().build().unwrap()),
//!         CliMode::CrashProbe => std::process::exit(nanobot_boot::cli::crash_probe_exit(make_builder)),
//!         CliMode::AgentSmoke => std::process::exit(nanobot_boot::cli::agent_smoke_exit(make_builder)),
//!         CliMode::AgentRun(id) => std::process::exit(nanobot_boot::cli::agent_run_exit(product_builder_for, &id)),
//!         CliMode::Run => product_gui::run(),
//!     }
//! }
//! ```

use std::path::PathBuf;

use crate::harness::run_one_turn;
use crate::HarnessBuilder;

/// The parsed command line.
#[derive(Clone, Debug)]
pub struct Cli {
    /// The run mode.
    pub mode: CliMode,
    /// Seconds for `--smoke [secs]`; `Some` only when `--smoke` was given.
    pub smoke_secs: Option<f32>,
    /// The crash-probe working directory (`NB_CRASH_DIR`). When set, the
    /// product should assemble its headless harness with the project paths
    /// pointing here — that is how the probe phases share one directory.
    pub crash_dir: Option<PathBuf>,
}

/// The run mode.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CliMode {
    /// Run the GUI (default).
    Run,
    /// Headless selfcheck.
    Selfcheck,
    /// Crash-recovery probe (invoked by the selfcheck as a subprocess).
    CrashProbe,
    /// Assemble + one headless turn + report (CI smoke of the agent path).
    AgentSmoke,
    /// Assemble for an existing session id + one headless turn.
    AgentRun(String),
}

impl Cli {
    /// Parses the process environment (args + `NB_CRASH_DIR`).
    pub fn from_env() -> Self {
        let args: Vec<String> = std::env::args().skip(1).collect();
        let mode = if has_flag(&args, "--selfcheck") {
            CliMode::Selfcheck
        } else if has_flag(&args, "--selfcheck-crash") {
            CliMode::CrashProbe
        } else if has_flag(&args, "--agent-smoke") {
            CliMode::AgentSmoke
        } else if let Some(id) = option_value(&args, "--agent-run") {
            CliMode::AgentRun(id)
        } else {
            CliMode::Run
        };
        let smoke_secs = args.iter().position(|a| a == "--smoke").map(|index| {
            args.get(index + 1)
                .and_then(|s| s.parse::<f32>().ok())
                .unwrap_or(2.5)
                .max(0.5)
        });
        let crash_dir = std::env::var("NB_CRASH_DIR").ok().map(PathBuf::from);
        Self {
            mode,
            smoke_secs,
            crash_dir,
        }
    }
}

fn has_flag(args: &[String], flag: &str) -> bool {
    args.iter().any(|a| a == flag)
}

/// `--flag value` from the CLI args.
fn option_value(args: &[String], flag: &str) -> Option<String> {
    let index = args.iter().position(|a| a == flag)?;
    args.get(index + 1).cloned()
}

/// Run one phase of the crash-recovery probe (the selfcheck's subprocess
/// mode) and return the process exit code.
///
/// The probe is a real crash simulation split across three subprocess launches:
///
/// - phase A (`NB_CRASH_PHASE=A`, the default): the harness drives a probe
///   turn, then exits **without running `Drop`** — so the `clean.marker` is
///   never written.
/// - phase B (`NB_CRASH_PHASE=B`): the build detects the missing marker,
///   hydrates the log, and verifies the recovered session's log bytes match
///   what phase A persisted. It returns normally, so `Drop` writes the marker.
/// - phase C (`NB_CRASH_PHASE=C`): the marker is present, so `build()` must
///   not report recovery even though the log is still there.
///
/// `make_builder` must assemble the product normally, with its project paths
/// redirected into the isolated directory given by `NB_CRASH_DIR`.
pub fn crash_probe_exit(make_builder: impl Fn() -> HarnessBuilder) -> i32 {
    let dir = PathBuf::from(
        std::env::var("NB_CRASH_DIR").expect("the NB_CRASH_DIR environment variable is required"),
    );
    let phase = std::env::var("NB_CRASH_PHASE").unwrap_or_default();
    match phase.as_str() {
        "B" => crate::selfcheck::crash_probe_recover(
            make_builder().build().expect("session B assembly failed"),
            &dir,
        ),
        "C" => crate::selfcheck::crash_probe_clean(
            make_builder().build().expect("session C assembly failed"),
            &dir,
        ),
        // Session A: the runtime is dropped by `std::process::exit`, i.e. not
        // at all — no clean marker is written.
        _ => crate::selfcheck::crash_probe_crash(
            make_builder().build().expect("session A assembly failed"),
            &dir,
        ),
    }
}

/// Assemble the harness, drive one headless turn, and report (exit code 0 =
/// the turn produced session events).
pub fn agent_smoke_exit(make_builder: impl Fn() -> HarnessBuilder) -> i32 {
    let harness = match make_builder().build() {
        Ok(harness) => harness,
        Err(error) => {
            println!("FAIL: harness assembly failed: {error}");
            return 1;
        }
    };
    run_one_turn(&harness, "hello");
    let count = harness.agent.session().len();
    if count > 0 {
        println!("PASS: agent produced {count} session events");
        0
    } else {
        println!("FAIL: agent produced no session events");
        1
    }
}

/// Assemble the harness for `session_id`, drive one headless turn, and report
/// the event count (a diagnostics mode, always exit 0).
pub fn agent_run_exit(
    make_builder: impl Fn(&str) -> HarnessBuilder,
    session_id: &str,
) -> i32 {
    let harness = match make_builder(session_id).build() {
        Ok(harness) => harness,
        Err(error) => {
            println!("FAIL: harness assembly for `{session_id}` failed: {error}");
            return 1;
        }
    };
    run_one_turn(&harness, "test");
    let count = harness.agent.session().len();
    println!("agent-run session {session_id}: {count} events");
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn with_args(args: &[&str]) -> Cli {
        // from_env reads the process args; the parse helpers are exercised
        // through them indirectly. Re-implement the scan here against the
        // same rules to keep the test hermetic.
        let owned: Vec<String> = args.iter().map(|s| s.to_string()).collect();
        let mode = if has_flag(&owned, "--selfcheck") {
            CliMode::Selfcheck
        } else if has_flag(&owned, "--selfcheck-crash") {
            CliMode::CrashProbe
        } else if has_flag(&owned, "--agent-smoke") {
            CliMode::AgentSmoke
        } else if let Some(id) = option_value(&owned, "--agent-run") {
            CliMode::AgentRun(id)
        } else {
            CliMode::Run
        };
        let smoke_secs = owned.iter().position(|a| a == "--smoke").map(|index| {
            owned
                .get(index + 1)
                .and_then(|s| s.parse::<f32>().ok())
                .unwrap_or(2.5)
                .max(0.5)
        });
        Cli {
            mode,
            smoke_secs,
            crash_dir: None,
        }
    }

    #[test]
    fn parses_every_mode() {
        assert_eq!(with_args(&[]).mode, CliMode::Run);
        assert_eq!(with_args(&["--project", "x"]).mode, CliMode::Run);
        assert_eq!(with_args(&["--selfcheck"]).mode, CliMode::Selfcheck);
        assert_eq!(with_args(&["--selfcheck-crash"]).mode, CliMode::CrashProbe);
        assert_eq!(with_args(&["--agent-smoke"]).mode, CliMode::AgentSmoke);
        assert_eq!(
            with_args(&["--agent-run", "s-1"]).mode,
            CliMode::AgentRun("s-1".to_string())
        );
    }

    #[test]
    fn smoke_seconds_default_and_parse() {
        assert_eq!(with_args(&["--smoke"]).smoke_secs, Some(2.5));
        assert_eq!(with_args(&["--smoke", "3"]).smoke_secs, Some(3.0));
        assert_eq!(with_args(&[]).smoke_secs, None);
    }
}
