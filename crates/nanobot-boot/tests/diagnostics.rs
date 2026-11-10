//! The diagnostics a product turns on from `main`, end to end: `init`
//! installs a process-wide logger and panic hook, so this lives in a test
//! binary of its own, as one test.
#![cfg(not(target_family = "wasm"))]

use nanobot_boot::diagnostics::{
    crash_reports, init, log_file, report, take_unseen_crash_reports, ReportInput,
};

#[test]
fn a_panic_leaves_a_report_and_the_log_reaches_the_file() {
    let dir = std::env::temp_dir().join(format!("nb-diag-e2e-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);

    init(&dir, "Acme Designer 2.1");
    log::warn!(target: "some_dependency", "disk nearly full");
    log::info!(target: "nanobot_boot", "assembled");
    log::info!(target: "some_dependency", "chatter");

    let log = std::fs::read_to_string(log_file(&dir)).expect("the log file exists");
    assert!(log.contains("Acme Designer 2.1 started"));
    assert!(log.contains("WARN  some_dependency: disk nearly full"));
    assert!(log.contains("INFO  nanobot_boot: assembled"));
    assert!(!log.contains("chatter"), "other crates only get warnings into the file");

    assert!(take_unseen_crash_reports(&dir).is_empty());
    let panicked = std::thread::Builder::new()
        .name("worker".to_string())
        .spawn(|| panic!("the mesher gave up"))
        .unwrap()
        .join();
    assert!(panicked.is_err());

    let reports = crash_reports(&dir);
    assert_eq!(reports.len(), 1);
    let crash = std::fs::read_to_string(&reports[0]).unwrap();
    assert!(crash.starts_with("Acme Designer 2.1, "));
    assert!(crash.contains("Thread worker panicked at "));
    assert!(crash.contains("diagnostics.rs"), "the location is there");
    assert!(crash.contains("the mesher gave up"));
    assert!(crash.contains("Backtrace:"));
    assert!(crash.contains("disk nearly full"), "the end of the log is attached");

    // The next launch hears about it once, and the export carries it.
    assert_eq!(take_unseen_crash_reports(&dir), reports);
    assert!(take_unseen_crash_reports(&dir).is_empty());
    let text = report(&ReportInput {
        product: "Acme Designer 2.1",
        dir: &dir,
        settings: None,
        sections: Vec::new(),
    });
    assert!(text.contains("the mesher gave up"));
    assert!(text.contains("ERROR nanobot: Thread worker panicked"));

    let _ = std::fs::remove_dir_all(&dir);
}
