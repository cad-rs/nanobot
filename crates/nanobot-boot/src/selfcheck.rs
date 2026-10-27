//! Headless selfcheck: drive the assembled harness through the kernel's key
//! paths and report PASS / FAIL / SKIP lines (process exit code 0/1).
//!
//! The checks are **probe-style**: a check whose precondition the product
//! does not meet is SKIP, never FAIL — any product can run the selfcheck,
//! and only what it actually assembled is verified (the gate is never bound
//! to a sample module's specifics).
//!
//! The crash-recovery check is a real crash simulation split across three
//! subprocess launches of the product binary (`--selfcheck-crash`): A exits
//! without `Drop`, B recovers and verifies the log byte-for-byte, C verifies
//! the clean marker suppresses recovery. A product that does not implement
//! the mode is caught by the subprocess timeout (FAIL, not a hang).

use std::path::Path;
use std::time::{Duration, Instant};

#[cfg(not(target_family = "wasm"))]
use crate::harness::is_user_message;
use crate::harness::run_one_turn;
use crate::Harness;

/// One selfcheck outcome.
#[derive(Debug, Clone)]
pub struct Check {
    pub name: String,
    pub ok: bool,
    pub skipped: bool,
    pub detail: String,
}

impl Check {
    pub fn new(name: impl Into<String>, ok: bool, detail: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            ok,
            skipped: false,
            detail: detail.into(),
        }
    }

    /// Precondition not met (probe-style: unrelated checks SKIP, not FAIL).
    pub fn skip(name: impl Into<String>, reason: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            ok: false,
            skipped: true,
            detail: reason.into(),
        }
    }
}

/// The message the selfcheck drives through the agent (the persistence probe
/// looks for it in the re-read log).
pub const SELFCHECK_TURN: &str = "selfcheck turn";

/// Runs the framework's checks against an assembled harness. Products append
/// their own items and pass the merged list to [`report`].
pub fn checks(harness: &Harness) -> Vec<Check> {
    let mut checks = Vec::new();

    // 1. Identity descriptor (probe-style: no harness.toml -> SKIP).
    match harness.profile_source() {
        Some(source) => checks.push(Check::new(
            "身份: harness.toml 已加载",
            true,
            format!("{} @ {}", harness.profile().id, source.display()),
        )),
        None => checks.push(Check::skip(
            "身份: harness.toml 已加载",
            "未找到 harness.toml（内置默认身份）",
        )),
    }

    // 2. Assembly: the tool backend (probe: only when tools are registered).
    if harness.tool_count() > 0 {
        checks.push(Check::new(
            "装配: 工具后端已挂接",
            harness.has_tool_executor(),
            format!(
                "tools={} executor={}",
                harness.tool_count(),
                harness.has_tool_executor()
            ),
        ));
    } else {
        checks.push(Check::skip("装配: 工具后端已挂接", "产品未注册工具定义"));
    }

    // 3. The turn loop (every harness runs: the scripted default guarantees
    //    an adapter).
    run_one_turn(harness, SELFCHECK_TURN);
    checks.push(Check::new(
        "会话: turn 事件落日志",
        !harness.agent.session().is_empty(),
        format!("events={}", harness.agent.session().len()),
    ));

    // 4. Persistence round trip (probe: needs project paths; native only —
    // the web has no file to read back, so the probe skips there).
    match harness.events_path() {
        Some(events) => checks.push(persistence_probe(&events)),
        None => checks.push(Check::skip(
            "持久化: 事件日志落盘可回读",
            "未配置项目路径",
        )),
    }

    // 5. The skill waterfall (probe: needs enabled skills).
    if !harness.skills.is_empty() {
        let prompt = harness.agent.system_prompt();
        checks.push(Check::new(
            "技能: prompt/assemble 瀑布生效",
            prompt.contains("[Skill:"),
            format!("skills={}", harness.skills.len()),
        ));
    } else {
        checks.push(Check::skip(
            "技能: prompt/assemble 瀑布生效",
            "未启用任何 skill",
        ));
    }

    // 6. Crash recovery: the three-phase subprocess probe (probe: needs
    //    project paths; a product without --selfcheck-crash times out ->
    //    FAIL, not a hang).
    if harness.paths().is_some() {
        let (ok, detail) = crash_probe_check();
        checks.push(Check::new("崩溃恢复: 三阶段子进程探针", ok, detail));
    } else {
        checks.push(Check::skip(
            "崩溃恢复: 三阶段子进程探针",
            "未配置项目路径（无会话持久化）",
        ));
    }

    // 7. Recovery repair (probe: only when the previous run crashed). A tool
    //    call without its result makes the log an invalid turn for every
    //    provider, so a recovered session must have none left.
    if harness.crash_recovered() {
        let dangling =
            nanobot_session::dangling_tool_calls(&harness.agent.session().events()).len();
        checks.push(Check::new(
            "崩溃恢复: 悬空调用已了结",
            dangling == 0,
            format!(
                "dangling={dangling} repaired={} pending_replay={}",
                harness.repaired_calls().len(),
                harness.pending_resume().len()
            ),
        ));
    } else {
        checks.push(Check::skip(
            "崩溃恢复: 悬空调用已了结",
            "上次运行正常结束（无恢复）",
        ));
    }

    checks
}

/// Prints the checks and returns the process exit code (0 = nothing failed;
/// SKIPs do not fail).
/// The persistence probe's read-back half. Native reads the JSONL log; the
/// web has no project file, so the probe skips (probe semantics: a missing
/// prerequisite is a SKIP, never a FAIL).
#[cfg(not(target_family = "wasm"))]
fn persistence_probe(events: &Path) -> Check {
    let found = nanobot_session::persistence::load(events)
        .map(|entries| {
            entries
                .iter()
                .any(|entry| is_user_message(&entry.event, SELFCHECK_TURN))
        })
        .unwrap_or(false);
    Check::new(
        "持久化: 事件日志落盘可回读",
        found,
        format!("path={}", events.display()),
    )
}

#[cfg(target_family = "wasm")]
fn persistence_probe(_events: &Path) -> Check {
    Check::skip(
        "持久化: 事件日志落盘可回读",
        "wasm 没有可回读的项目文件",
    )
}

pub fn report(checks: &[Check]) -> i32 {
    let failed = checks.iter().filter(|c| !c.ok && !c.skipped).count();
    let skipped = checks.iter().filter(|c| c.skipped).count();
    for check in checks {
        let tag = if check.skipped {
            "SKIP"
        } else if check.ok {
            "PASS"
        } else {
            "FAIL"
        };
        println!("[{}] {} — {}", tag, check.name, check.detail);
    }
    let run_total = checks.len() - skipped;
    println!(
        "selfcheck: {}/{} 项通过（{} 项跳过）",
        run_total - failed,
        run_total,
        skipped
    );
    if failed == 0 {
        0
    } else {
        1
    }
}

/// Headless selfcheck and exit (for a product `main`'s `--selfcheck` mode).
pub fn exit_with(harness: &Harness) -> ! {
    std::process::exit(report(&checks(harness)))
}

/// The headless selfcheck on a harness (`--selfcheck`): the framework suite
/// plus the process exit code.
pub fn run(harness: &Harness) -> i32 {
    report(&checks(harness))
}

/// Session A of the crash-recovery probe (`NB_CRASH_PHASE=A`): drive a probe
/// turn, snapshot the persisted log bytes, then exit **without running
/// `Drop`** — so the clean marker is never written and the next launch must
/// recover.
pub fn crash_probe_crash(harness: Harness, dir: &Path) -> ! {
    run_one_turn(&harness, "crash-probe turn");
    let events = harness
        .events_path()
        .expect("the probe harness has project paths");
    let expected = std::fs::read(&events).ok();
    let _ = std::fs::create_dir_all(dir);
    if let Some(bytes) = &expected {
        let _ = std::fs::write(dir.join("expected.bin"), bytes);
    }
    let _ = std::fs::write(
        dir.join("probe.json"),
        serde_json::json!({
            "session_id": harness.session_id(),
            "events": harness.agent.session().len(),
        })
        .to_string(),
    );
    println!(
        "[crash-probe] session A: events={} session={}",
        harness.agent.session().len(),
        harness.session_id()
    );
    use std::io::Write as _;
    let _ = std::io::stdout().flush();
    // Abnormal exit: no Drop, therefore no clean marker.
    std::process::exit(if expected.is_some() { 0 } else { 1 });
}

/// Session B of the crash-recovery probe (`NB_CRASH_PHASE=B`): the launch
/// after the simulated crash. `build()` has already hydrated the log and
/// flagged the missing marker; verify the flag and that the recovered log's
/// bytes match what session A persisted. Returning normally lets `Drop`
/// write the marker — that clean exit is exactly what session C checks.
pub fn crash_probe_recover(harness: Harness, dir: &Path) -> i32 {
    let expected = std::fs::read(dir.join("expected.bin")).ok();
    let events = harness
        .events_path()
        .expect("the probe harness has project paths");
    let current = std::fs::read(&events).ok();
    let content_ok = harness.crash_recovered()
        && expected.is_some()
        && current.as_deref() == expected.as_deref();
    let _ = std::fs::write(
        dir.join("result.json"),
        serde_json::json!({
            "recovered": harness.crash_recovered(),
            "content_ok": content_ok,
            "events": harness.agent.session().len(),
        })
        .to_string(),
    );
    println!(
        "[crash-probe] session B: recovered={} content_ok={} events={}",
        harness.crash_recovered(),
        content_ok,
        harness.agent.session().len()
    );
    if content_ok {
        0
    } else {
        1
    }
}

/// Session C of the crash-recovery probe (`NB_CRASH_PHASE=C`): the launch
/// after a *clean* exit, so the marker is present and `build()` must not
/// report recovery — even though the log is still there. Writes `clean.json`.
pub fn crash_probe_clean(harness: Harness, dir: &Path) -> i32 {
    let suppressed = !harness.crash_recovered();
    let _ = std::fs::write(
        dir.join("clean.json"),
        serde_json::json!({
            "recovered": harness.crash_recovered(),
            "suppressed": suppressed,
        })
        .to_string(),
    );
    println!(
        "[crash-probe] session C: recovered={} suppressed={}",
        harness.crash_recovered(),
        suppressed
    );
    if suppressed {
        0
    } else {
        1
    }
}

/// Launches one crash-probe phase as a subprocess. Times out instead of
/// hanging when a product does not implement the `--selfcheck-crash` mode.
fn run_probe_phase(
    exe: &std::path::Path,
    dir: &std::path::Path,
    phase: &str,
) -> Result<std::process::Output, String> {
    use std::process::{Command, Stdio};
    let mut child = Command::new(exe)
        .arg("--selfcheck-crash")
        .env("NB_CRASH_DIR", dir)
        .env("NB_CRASH_PHASE", phase)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("无法启动子进程: {e}"))?;
    let start = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {
                if start.elapsed() > Duration::from_secs(30) {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(format!("子进程超时（phase {phase}）"));
                }
                std::thread::sleep(Duration::from_millis(20));
            }
            Err(e) => return Err(format!("等待子进程失败: {e}")),
        }
    };
    let mut out = child
        .wait_with_output()
        .map_err(|e| format!("读取子进程输出失败: {e}"))?;
    out.status = status;
    Ok(out)
}

/// The crash-recovery subprocess check (returns (passed, detail)).
///
/// Three subprocess launches cover the discipline's two directions:
/// - A: drive a probe turn, then exit without `Drop` (no clean marker);
/// - B: build recovers, and the hydrated log matches A's bytes;
/// - C: marker present -> recovery must be suppressed.
fn crash_probe_check() -> (bool, String) {
    let dir = std::env::temp_dir().join("nanobot-crash-probe");
    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::create_dir_all(&dir);
    let mut ok = false;
    let detail = match std::env::current_exe() {
        Err(_) => "无法定位当前 exe".to_string(),
        Ok(exe) => {
            let mut outcome: Result<(), String> = Ok(());
            for phase in ["A", "B", "C"] {
                outcome = match run_probe_phase(&exe, &dir, phase) {
                    Err(e) => Err(e),
                    Ok(output) if !output.status.success() => Err(format!(
                        "会话 {phase} 失败 status={:?} stdout={} stderr={}",
                        output.status,
                        String::from_utf8_lossy(&output.stdout),
                        String::from_utf8_lossy(&output.stderr)
                    )),
                    Ok(_) => Ok(()),
                };
                if outcome.is_err() {
                    break;
                }
            }
            match outcome {
                Err(e) => e,
                Ok(()) => {
                    let result = read_json(&dir.join("result.json"));
                    let clean = read_json(&dir.join("clean.json"));
                    ok = result.get("recovered").and_then(|v| v.as_bool()) == Some(true)
                        && result.get("content_ok").and_then(|v| v.as_bool()) == Some(true)
                        && clean.get("suppressed").and_then(|v| v.as_bool()) == Some(true);
                    format!(
                        "recovered={:?} content_ok={:?} clean_exit_suppressed={:?}",
                        result.get("recovered").and_then(|v| v.as_bool()),
                        result.get("content_ok").and_then(|v| v.as_bool()),
                        clean.get("suppressed").and_then(|v| v.as_bool())
                    )
                }
            }
        }
    };
    (ok, detail)
}

fn read_json(path: &std::path::Path) -> serde_json::Value {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or(serde_json::Value::Null)
}
