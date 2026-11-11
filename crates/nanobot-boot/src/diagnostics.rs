//! What support needs when something goes wrong on a customer's machine: a
//! rolling log file, a report for every panic, and the plain-text export the
//! user hands over.
//!
//! A product calls [`init`] once from `main` with its config directory
//! ([`crate::settings::user_config_dir`]). From then on everything logged
//! through the `log` facade — the framework's own lines and gpui's — also
//! goes to `logs/app.log` there, and a panic writes
//! `crashes/crash-<UTC time>-….txt` with the location, the backtrace and the
//! end of the log. The next launch learns about it from
//! [`take_unseen_crash_reports`], once.
//!
//! Nothing leaves the machine. [`report`] builds the text the user can read
//! before sending it; a product that wants reports submitted injects a
//! [`CrashReporter`] and offers the [`SEND_CRASH_REPORTS`] setting, which is
//! off until the user turns it on ([`submit_crash_reports`]).
//!
//! The log file mechanics, the crash-report text and the clock live in
//! `product_kit` (shared with the workbench framework); this module keeps the
//! harness-specific parts: the logger, the file backend and the report input.
//!
//! Only panics are captured, not native crashes. On wasm there is no file to
//! write: [`init`] does not exist and [`report`] carries no log.

use std::env::consts::{ARCH, OS};
use std::path::{Path, PathBuf};

use product_kit::diagnostics::{section, tail, REPORTED_LOG_BYTES};

use crate::Settings;

#[cfg(not(target_family = "wasm"))]
pub use native::init;

/// The folder under the config directory that crash reports live in. Their
/// names start with the UTC time, so they sort oldest first.
pub const CRASH_DIR: &str = "crashes";
/// The folder under the config directory that the log files live in.
pub const LOG_DIR: &str = "logs";
/// The setting a product with a [`CrashReporter`] offers; off until the user
/// turns it on.
pub const SEND_CRASH_REPORTS: &str = "diagnostics.send_crash_reports";
/// How many crash reports are kept.
#[cfg(not(target_family = "wasm"))]
const KEPT_REPORTS: usize = 20;
/// How many crash reports [`report`] includes, newest first.
const REPORTED_CRASHES: usize = 5;
/// The file that remembers the newest crash report a launch has announced.
const SEEN_MARKER: &str = ".seen";

/// The current log file under `dir`.
pub fn log_file(dir: &Path) -> PathBuf {
    dir.join(LOG_DIR).join("app.log")
}

/// The crash reports under `dir`, oldest first.
pub fn crash_reports(dir: &Path) -> Vec<PathBuf> {
    let mut reports: Vec<PathBuf> = std::fs::read_dir(dir.join(CRASH_DIR))
        .into_iter()
        .flatten()
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| {
            path.extension().is_some_and(|ext| ext == "txt")
                && file_name(path).starts_with("crash-")
        })
        .collect();
    reports.sort();
    reports
}

/// The crash reports written since the last call, oldest first: what a launch
/// tells the user about, once. Calling it marks them seen.
pub fn take_unseen_crash_reports(dir: &Path) -> Vec<PathBuf> {
    let marker = dir.join(CRASH_DIR).join(SEEN_MARKER);
    let seen = std::fs::read_to_string(&marker).unwrap_or_default();
    let unseen: Vec<PathBuf> = crash_reports(dir)
        .into_iter()
        .filter(|path| file_name(path).as_str() > seen.trim())
        .collect();
    if let Some(newest) = unseen.last() {
        let _ = crate::write_whole(&marker, file_name(newest).as_bytes());
    }
    unseen
}

/// Where a product sends crash reports, if it sends them at all.
pub trait CrashReporter: Send + Sync {
    /// Submits one report's text. Called off the hot path, at launch.
    fn send(&self, report: &str) -> Result<(), String>;
}

/// Sends `reports` through `reporter` if the user turned
/// [`SEND_CRASH_REPORTS`] on; returns how many went out. With the setting
/// off — the default — nothing is read and nothing is sent.
pub fn submit_crash_reports(
    reporter: &dyn CrashReporter,
    settings: &Settings,
    reports: &[PathBuf],
) -> usize {
    let enabled = settings
        .get(SEND_CRASH_REPORTS)
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);
    if !enabled {
        return 0;
    }
    reports
        .iter()
        .filter_map(|path| std::fs::read_to_string(path).ok())
        .filter(|text| reporter.send(text).is_ok())
        .count()
}

/// What goes into [`report`].
pub struct ReportInput<'a> {
    /// The product's name and version, as the user knows them.
    pub product: &'a str,
    /// The product's config directory (the one [`init`] was given).
    pub dir: &'a Path,
    /// The user settings; secrets and plugin settings are left out.
    pub settings: Option<&'a Settings>,
    /// The product's own sections, in order: a title and its lines (session
    /// shape, plugins loaded and rejected, assembly notes).
    pub sections: Vec<(String, Vec<String>)>,
}

/// Everything support asks for first, as plain text so the user can read what
/// they are about to send. Sign-in tokens (`auth.*`) and plugin settings
/// (`plugin.*`) stay out.
pub fn report(input: &ReportInput<'_>) -> String {
    let mut lines = vec![
        input.product.to_string(),
        format!("Framework: nanobot {}", env!("CARGO_PKG_VERSION")),
        format!("System: {OS} {ARCH}"),
    ];
    for (title, body) in &input.sections {
        lines.push(section(title));
        lines.extend(body.iter().cloned());
    }
    if let Some(settings) = input.settings {
        lines.push(section("Settings"));
        lines.extend(
            settings
                .iter()
                .filter(|(key, _)| !key.starts_with("auth.") && !key.starts_with("plugin."))
                .map(|(key, value)| format!("{key} = {value}")),
        );
    }
    lines.push(section("Log"));
    lines.push(tail(&log_file(input.dir), REPORTED_LOG_BYTES));
    for path in crash_reports(input.dir).iter().rev().take(REPORTED_CRASHES) {
        lines.push(section(&file_name(path)));
        lines.push(std::fs::read_to_string(path).unwrap_or_default());
    }
    lines.join("\n")
}

fn file_name(path: &Path) -> String {
    path.file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned()
}

#[cfg(not(target_family = "wasm"))]
mod native {
    use std::fs;
    use std::path::Path;
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::sync::{Mutex, PoisonError};

    use product_kit::diagnostics::{
        crash_report, now_secs, panic_message, tail, utc, utc_rfc3339, utc_stamp, RollingFile,
        CRASH_LOG_BYTES, MAX_LOG_BYTES, MAX_REPORTS_PER_SESSION,
    };

    use super::{crash_reports, log_file, CRASH_DIR, KEPT_REPORTS};

    /// Starts writing the log to a rolling file under `dir` (`logs/app.log`)
    /// and a report for every panic (`crashes/`). Call it once, from `main`,
    /// with the product's config directory and its name and version.
    ///
    /// The file gets warnings, plus information from the `nanobot*` crates;
    /// stderr gets warnings. `RUST_LOG` set to a level name (`debug`, say)
    /// replaces both thresholds. A product that installed its own logger
    /// keeps it and gets the crash reports only.
    pub fn init(dir: &Path, product: &str) {
        prune_crash_reports(dir);
        install_logger(&log_file(dir));
        install_panic_hook(
            format!("{product}, {} {}", std::env::consts::OS, std::env::consts::ARCH),
            dir.to_path_buf(),
        );
        log::info!(target: "nanobot", "==== {product} started, pid {} ====", std::process::id());
    }

    /// Drops the oldest reports beyond [`KEPT_REPORTS`].
    fn prune_crash_reports(dir: &Path) {
        let reports = crash_reports(dir);
        let excess = reports.len().saturating_sub(KEPT_REPORTS);
        for path in &reports[..excess] {
            let _ = fs::remove_file(path);
        }
    }

    struct Logger {
        /// What stderr shows.
        stderr_level: log::LevelFilter,
        /// What the file takes from any crate, and from the framework's own.
        file_level: log::LevelFilter,
        own_level: log::LevelFilter,
        file: Mutex<RollingFile>,
    }

    impl Logger {
        fn file_takes(&self, metadata: &log::Metadata) -> bool {
            let level = if metadata.target().starts_with("nanobot") {
                self.own_level
            } else {
                self.file_level
            };
            metadata.level() <= level
        }
    }

    impl log::Log for Logger {
        fn enabled(&self, metadata: &log::Metadata) -> bool {
            metadata.level() <= self.stderr_level || self.file_takes(metadata)
        }

        fn log(&self, record: &log::Record) {
            if record.level() <= self.stderr_level {
                eprintln!("{:<5} {}: {}", record.level(), record.target(), record.args());
            }
            if self.file_takes(record.metadata()) {
                let time = utc(now_secs());
                let line = format!(
                    "{} {:<5} {}: {}\n",
                    utc_rfc3339(time),
                    record.level(),
                    record.target(),
                    record.args()
                );
                self.file
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .write_line(&line);
            }
        }

        fn flush(&self) {}
    }

    fn install_logger(log_file: &Path) {
        let asked = std::env::var("RUST_LOG")
            .ok()
            .and_then(|level| level.parse::<log::LevelFilter>().ok());
        let logger = Logger {
            stderr_level: asked.unwrap_or(log::LevelFilter::Warn),
            file_level: asked.unwrap_or(log::LevelFilter::Warn),
            own_level: asked.unwrap_or(log::LevelFilter::Info),
            file: Mutex::new(RollingFile::open(log_file.to_path_buf(), MAX_LOG_BYTES)),
        };
        let max_level = logger.own_level.max(logger.stderr_level);
        if log::set_boxed_logger(Box::new(logger)).is_ok() {
            log::set_max_level(max_level);
        }
    }

    fn install_panic_hook(product: String, dir: std::path::PathBuf) {
        static WRITTEN: AtomicU32 = AtomicU32::new(0);
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            let panic = panic_message(&info);
            let written = WRITTEN.fetch_add(1, Ordering::Relaxed);
            if written < MAX_REPORTS_PER_SESSION {
                let time = utc(now_secs());
                let path = dir.join(CRASH_DIR).join(format!(
                    "crash-{}-{}-{written}.txt",
                    utc_stamp(time),
                    std::process::id()
                ));
                let backtrace = std::backtrace::Backtrace::force_capture();
                let report = crash_report(
                    &product,
                    &utc_rfc3339(time),
                    &panic,
                    &backtrace.to_string(),
                    &tail(&log_file(&dir), CRASH_LOG_BYTES),
                );
                let _ = fs::create_dir_all(dir.join(CRASH_DIR));
                let _ = fs::write(path, report);
            }
            log::error!(target: "nanobot", "{panic}");
            previous(info);
        }));
    }
}

#[cfg(all(test, not(target_family = "wasm")))]
mod tests {
    use super::*;
    use std::fs;
    use std::sync::Mutex;

    fn empty_dir(test: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("nb-diag-{}-{test}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn crash(dir: &Path, name: &str, text: &str) {
        fs::create_dir_all(dir.join(CRASH_DIR)).unwrap();
        fs::write(dir.join(CRASH_DIR).join(name), text).unwrap();
    }

    #[test]
    fn a_launch_hears_about_a_crash_once() {
        let dir = empty_dir("unseen");
        assert!(take_unseen_crash_reports(&dir).is_empty());
        crash(&dir, "crash-20261109-101500-1-0.txt", "first");
        crash(&dir, "crash-20261109-101501-1-1.txt", "second");
        assert_eq!(take_unseen_crash_reports(&dir).len(), 2);
        assert!(take_unseen_crash_reports(&dir).is_empty(), "already told");
        crash(&dir, "crash-20261110-090000-2-0.txt", "third");
        let unseen = take_unseen_crash_reports(&dir);
        assert_eq!(unseen.len(), 1);
        assert!(unseen[0].ends_with("crash-20261110-090000-2-0.txt"));
        assert_eq!(crash_reports(&dir).len(), 3, "the marker is not a report");
        let _ = fs::remove_dir_all(&dir);
    }

    struct Recorder(Mutex<Vec<String>>);

    impl CrashReporter for Recorder {
        fn send(&self, report: &str) -> Result<(), String> {
            self.0.lock().unwrap().push(report.to_string());
            Ok(())
        }
    }

    #[test]
    fn reports_are_sent_only_after_the_user_turned_it_on() {
        let dir = empty_dir("submit");
        crash(&dir, "crash-20261109-101500-1-0.txt", "what happened");
        let reports = crash_reports(&dir);
        let recorder = Recorder(Mutex::new(Vec::new()));
        let mut settings = Settings::in_memory();

        assert_eq!(submit_crash_reports(&recorder, &settings, &reports), 0);
        assert!(recorder.0.lock().unwrap().is_empty(), "off by default");

        settings
            .set(SEND_CRASH_REPORTS, serde_json::Value::Bool(true))
            .unwrap();
        assert_eq!(submit_crash_reports(&recorder, &settings, &reports), 1);
        assert_eq!(*recorder.0.lock().unwrap(), ["what happened"]);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_report_shows_what_support_needs_and_no_secrets() {
        let dir = empty_dir("report");
        fs::create_dir_all(dir.join(LOG_DIR)).unwrap();
        fs::write(log_file(&dir), "WARN  nanobot: something odd\n").unwrap();
        for n in 0..7 {
            crash(&dir, &format!("crash-2026110{n}-000000-1-0.txt"), &format!("panic {n}"));
        }
        let mut settings = Settings::in_memory();
        settings.set("ui.theme", "dark".into()).unwrap();
        settings
            .set("auth.oauth.main.access", "secret-token".into())
            .unwrap();
        settings.set("plugin.acme.key", "private".into()).unwrap();

        let text = report(&ReportInput {
            product: "Acme Designer 2.1",
            dir: &dir,
            settings: Some(&settings),
            sections: vec![("Plugins".to_string(), vec!["acme.tools: loaded".to_string()])],
        });

        assert!(text.starts_with("Acme Designer 2.1\nFramework: nanobot "));
        assert!(text.contains("---- Plugins ----\nacme.tools: loaded"));
        assert!(text.contains("ui.theme = \"dark\""));
        assert!(!text.contains("secret-token") && !text.contains("private"));
        assert!(text.contains("something odd"));
        assert!(text.contains("panic 6") && text.contains("panic 2"), "the newest five");
        assert!(!text.contains("panic 1"), "older ones stay out");
        let _ = fs::remove_dir_all(&dir);
    }
}
