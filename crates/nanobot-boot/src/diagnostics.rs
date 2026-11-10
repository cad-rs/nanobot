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
//! Only panics are captured, not native crashes. On wasm there is no file to
//! write: [`init`] does not exist and [`report`] carries no log.
//!
//! Ported from workbench-rs's `workbench-core/src/diagnostics.rs`.

use std::env::consts::{ARCH, OS};
use std::path::{Path, PathBuf};

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
/// How much of the log [`report`] includes.
const REPORTED_LOG_BYTES: u64 = 1024 * 1024;
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

fn section(title: &str) -> String {
    format!("\n---- {title} ----")
}

fn file_name(path: &Path) -> String {
    path.file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned()
}

/// At most the last `limit` bytes of the file, starting at a whole line.
fn tail(path: &Path, limit: u64) -> String {
    use std::io::{Read as _, Seek as _, SeekFrom};

    let Ok(mut file) = std::fs::File::open(path) else {
        return String::new();
    };
    let start = file
        .metadata()
        .map_or(0, |meta| meta.len())
        .saturating_sub(limit);
    let mut bytes = Vec::new();
    if file.seek(SeekFrom::Start(start)).is_err() || file.read_to_end(&mut bytes).is_err() {
        return String::new();
    }
    let text = String::from_utf8_lossy(&bytes);
    match text.find('\n') {
        Some(end_of_cut_line) if start > 0 => text[end_of_cut_line + 1..].to_string(),
        _ => text.into_owned(),
    }
}

/// `secs` since the unix epoch as UTC `(year, month, day, hour, minute,
/// second)`. Hand-rolled (the civil-from-days algorithm) to keep a date crate
/// out of the kernel.
#[cfg(not(target_family = "wasm"))]
fn utc(secs: u64) -> (i64, u32, u32, u32, u32, u32) {
    let days = (secs / 86_400) as i64;
    let rest = (secs % 86_400) as u32;
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let day_of_era = z.rem_euclid(146_097);
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let shifted_month = (5 * day_of_year + 2) / 153;
    let day = (day_of_year - (153 * shifted_month + 2) / 5 + 1) as u32;
    let month = (if shifted_month < 10 {
        shifted_month + 3
    } else {
        shifted_month - 9
    }) as u32;
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    (year, month, day, rest / 3600, rest % 3600 / 60, rest % 60)
}

#[cfg(not(target_family = "wasm"))]
mod native {
    use std::fs::{self, File, OpenOptions};
    use std::io::Write as _;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::sync::{Mutex, PoisonError};
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::{crash_reports, log_file, tail, utc, CRASH_DIR, KEPT_REPORTS};

    /// A log file rolls over at this size…
    const MAX_LOG_BYTES: u64 = 5 * 1024 * 1024;
    /// …and this many older ones stay (`app.1.log` is the newest of them).
    const KEPT_LOGS: u32 = 3;
    /// How much of the log a crash report includes.
    const CRASH_LOG_BYTES: u64 = 64 * 1024;
    /// A panic storm stops writing reports here; the first are the telling ones.
    const MAX_REPORTS_PER_SESSION: u32 = 5;

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

    fn now_secs() -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |since| since.as_secs())
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
                let (year, month, day, hour, minute, second) = utc(now_secs());
                let line = format!(
                    "{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}Z {:<5} {}: {}\n",
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

    fn install_panic_hook(product: String, dir: PathBuf) {
        static WRITTEN: AtomicU32 = AtomicU32::new(0);
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            let thread = std::thread::current();
            let panic = format!(
                "Thread {} panicked at {}: {}",
                thread.name().unwrap_or("(unnamed)"),
                info.location()
                    .map_or("(unknown)".to_string(), ToString::to_string),
                info.payload_as_str().unwrap_or("(no message)"),
            );
            let written = WRITTEN.fetch_add(1, Ordering::Relaxed);
            if written < MAX_REPORTS_PER_SESSION {
                let (year, month, day, hour, minute, second) = utc(now_secs());
                let stamp = format!("{year:04}{month:02}{day:02}-{hour:02}{minute:02}{second:02}");
                let path = dir.join(CRASH_DIR).join(format!(
                    "crash-{stamp}-{}-{written}.txt",
                    std::process::id()
                ));
                let report = format!(
                    "{product}\nTime: {year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}Z\n\
                     {panic}\n\nBacktrace:\n{}\nRecent log:\n{}",
                    std::backtrace::Backtrace::force_capture(),
                    tail(&log_file(&dir), CRASH_LOG_BYTES),
                );
                let _ = fs::create_dir_all(dir.join(CRASH_DIR));
                let _ = fs::write(path, report);
            }
            log::error!(target: "nanobot", "{panic}");
            previous(info);
        }));
    }

    /// An append-only file that starts over when full, keeping the last few.
    /// Logging must never take the app down, so failures only lose lines.
    pub(super) struct RollingFile {
        path: PathBuf,
        file: Option<File>,
        size: u64,
        max_size: u64,
    }

    impl RollingFile {
        pub(super) fn open(path: PathBuf, max_size: u64) -> Self {
            if let Some(dir) = path.parent() {
                let _ = fs::create_dir_all(dir);
            }
            let mut rolling = Self {
                path,
                file: None,
                size: 0,
                max_size,
            };
            rolling.reopen();
            rolling
        }

        fn reopen(&mut self) {
            self.file = OpenOptions::new()
                .create(true)
                .append(true)
                .open(&self.path)
                .ok();
            self.size = self
                .file
                .as_ref()
                .and_then(|f| f.metadata().ok())
                .map_or(0, |m| m.len());
        }

        pub(super) fn write_line(&mut self, line: &str) {
            if self.size > 0 && self.size + line.len() as u64 > self.max_size {
                self.roll_over();
            }
            if let Some(file) = &mut self.file {
                if file.write_all(line.as_bytes()).is_ok() {
                    self.size += line.len() as u64;
                }
            }
        }

        fn roll_over(&mut self) {
            self.file = None; // Windows can't rename a file that is open
            for older in (1..KEPT_LOGS).rev() {
                let _ = fs::rename(self.numbered(older), self.numbered(older + 1));
            }
            let _ = fs::rename(&self.path, self.numbered(1));
            self.reopen();
        }

        /// `app.log` → `app.<n>.log`.
        fn numbered(&self, n: u32) -> PathBuf {
            let stem = self.path.file_stem().unwrap_or_default().to_string_lossy();
            self.path.with_file_name(format!("{stem}.{n}.log"))
        }
    }
}

#[cfg(all(test, not(target_family = "wasm")))]
mod tests {
    use super::native::RollingFile;
    use super::*;
    use std::fs;
    use std::sync::Mutex;

    fn empty_dir(test: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("nb-diag-{}-{test}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn read(dir: &Path, name: &str) -> String {
        fs::read_to_string(dir.join(name)).unwrap()
    }

    fn crash(dir: &Path, name: &str, text: &str) {
        fs::create_dir_all(dir.join(CRASH_DIR)).unwrap();
        fs::write(dir.join(CRASH_DIR).join(name), text).unwrap();
    }

    #[test]
    fn a_full_log_rolls_over_and_the_oldest_is_dropped() {
        let dir = empty_dir("roll");
        let mut log = RollingFile::open(dir.join("app.log"), 20);
        for n in 0..7 {
            log.write_line(&format!("line {n} of the log\n"));
        }
        assert_eq!(read(&dir, "app.log"), "line 6 of the log\n");
        assert_eq!(read(&dir, "app.1.log"), "line 5 of the log\n");
        assert_eq!(read(&dir, "app.2.log"), "line 4 of the log\n");
        assert_eq!(read(&dir, "app.3.log"), "line 3 of the log\n");
        assert_eq!(fs::read_dir(&dir).unwrap().count(), 4, "nothing older is kept");
        drop(log);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_new_session_continues_the_log_and_counts_what_is_there() {
        let dir = empty_dir("continue");
        fs::write(dir.join("app.log"), "from the last session\n").unwrap();
        let mut log = RollingFile::open(dir.join("app.log"), 30);
        log.write_line("short\n");
        assert_eq!(read(&dir, "app.log"), "from the last session\nshort\n");
        log.write_line("this one does not fit\n");
        assert_eq!(read(&dir, "app.log"), "this one does not fit\n");
        assert_eq!(read(&dir, "app.1.log"), "from the last session\nshort\n");
        drop(log);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_log_that_cannot_be_opened_only_loses_lines() {
        let dir = empty_dir("unwritable");
        fs::create_dir(dir.join("app.log")).unwrap();
        let mut log = RollingFile::open(dir.join("app.log"), 20);
        log.write_line("nowhere to go\n");
        drop(log);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_tail_starts_at_a_whole_line() {
        let dir = empty_dir("tail");
        let file = dir.join("log");
        fs::write(&file, "first line\nsecond line\nthird\n").unwrap();
        assert_eq!(tail(&file, 1000), "first line\nsecond line\nthird\n");
        assert_eq!(tail(&file, 15), "third\n");
        assert_eq!(tail(&dir.join("missing"), 15), "");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn utc_reads_the_calendar() {
        assert_eq!(utc(0), (1970, 1, 1, 0, 0, 0));
        assert_eq!(utc(951_782_400), (2000, 2, 29, 0, 0, 0), "a leap day");
        assert_eq!(utc(1_794_268_799), (2026, 11, 9, 23, 59, 59));
        assert_eq!(utc(4_102_444_800), (2100, 1, 1, 0, 0, 0));
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
