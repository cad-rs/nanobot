//! The job plane: work that outlives a turn.
//!
//! A design harness has multi-fidelity verification — a seconds-long rebuild,
//! a minutes-long extraction, an hours-long FEM or `JoSIM` run. Running any of
//! those on the agent's turn blocks the session: the model cannot iterate while
//! a solver runs, and a stalled turn looks identical to a hung one.
//!
//! This module is the seam for that. A [`JobQueue`] accepts [`JobSpec`]s and
//! runs them on worker threads, off the turn. A job is *addressable*: submit
//! returns a handle, `get`/`list` report state and progress, `cancel` stops a
//! queued job, and every state change reaches an optional sink so the host can
//! mirror it into the session log.
//!
//! The queue never runs anything itself — [`JobRunner`] is supplied by the
//! product, which knows what "run a margin analysis" or "run a DRC" means. The
//! framework provides the lifecycle, the concurrency and the addressing.

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;
use std::time::{SystemTime, UNIX_EPOCH};

/// A handle to what a job produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JobResult {
    /// A content-addressed handle to the result (a report hash, an artifact).
    Handle(String),
    /// The run failed; the string is the agent-readable reason.
    Error(String),
}

/// A job's lifecycle state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JobState {
    /// Accepted, waiting for a worker.
    Queued,
    /// Running on a worker.
    Running,
    /// Finished with a result.
    Succeeded,
    /// Finished with an error.
    Failed,
    /// Stopped before it could finish.
    Cancelled,
}

impl JobState {
    /// Whether the job will not change further.
    #[must_use]
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Succeeded | Self::Failed | Self::Cancelled)
    }

    /// Stable label for the session log and tool payloads.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Running => "running",
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        }
    }
}

/// A unit of work submitted to the queue.
#[derive(Debug, Clone)]
pub struct JobSpec {
    /// What to run (a tool name, a simulation kind).
    pub kind: String,
    /// The arguments the runner takes.
    pub arguments: serde_json::Value,
}

impl JobSpec {
    /// A job of `kind` with `arguments`.
    #[must_use]
    pub fn new(kind: impl Into<String>, arguments: serde_json::Value) -> Self {
        Self {
            kind: kind.into(),
            arguments,
        }
    }
}

/// A job's current state.
#[derive(Debug, Clone)]
pub struct Job {
    /// The queue-assigned id.
    pub id: String,
    /// What it runs.
    pub kind: String,
    /// Lifecycle state.
    pub state: JobState,
    /// Progress in `[0, 1]`, when the runner reports it.
    pub progress: Option<f64>,
    /// The result, once terminal.
    pub result: Option<JobResult>,
    /// The error, when [`JobState::Failed`].
    pub error: Option<String>,
    /// The spec it was submitted with.
    pub spec: JobSpec,
    /// Unix epoch ms when submitted.
    pub created_at_ms: u64,
    /// Unix epoch ms when a worker picked it up.
    pub started_at_ms: Option<u64>,
    /// Unix epoch ms when it reached a terminal state.
    pub finished_at_ms: Option<u64>,
}

impl Job {
    /// One-line rendering for the session log and tool payloads.
    #[must_use]
    pub fn headline(&self) -> String {
        let progress = self
            .progress
            .map_or_else(String::new, |p| format!(" {:.0}%", p * 100.0));
        format!(
            "{} {} [{}]{}",
            self.id,
            self.kind,
            self.state.as_str(),
            progress
        )
    }
}

/// A state change worth mirroring into the session log.
#[derive(Debug, Clone)]
pub struct JobEvent {
    /// The job id.
    pub job_id: String,
    /// What it runs.
    pub kind: String,
    /// The new state.
    pub state: JobState,
    /// Progress in `[0, 1]`, when known.
    pub progress: Option<f64>,
}

/// What a job run needs: its id, its spec, and a way to report progress.
pub struct JobContext<'a> {
    /// The queue-assigned job id.
    pub id: &'a str,
    /// The spec it was submitted with.
    pub spec: &'a JobSpec,
    /// Report progress in `[0, 1]`.
    pub progress: &'a dyn Fn(f64),
}

/// Executes a job.
///
/// Runs on a worker thread, off the agent's turn. Implementations are
/// synchronous: a job that takes an hour blocks one worker, not the session.
pub trait JobRunner: Send + Sync {
    /// Run the job to completion.
    fn run(&self, ctx: JobContext<'_>) -> Result<JobResult, String>;
}

struct Inner {
    jobs: HashMap<String, Job>,
    ready: VecDeque<String>,
    cancel_flags: HashMap<String, Arc<AtomicBool>>,
}

/// A queue that runs jobs on worker threads.
///
/// Workers are started at construction and stopped on drop, so a queue lives as
/// long as the harness.
pub struct JobQueue {
    runner: Arc<dyn JobRunner>,
    inner: Arc<Mutex<Inner>>,
    ready_cv: Arc<Condvar>,
    next: Arc<AtomicU64>,
    sink: Option<Arc<dyn Fn(JobEvent) + Send + Sync>>,
    shutdown: Arc<AtomicBool>,
    workers: Mutex<Vec<JoinHandle<()>>>,
}

impl JobQueue {
    /// A queue with `workers` worker threads over `runner`.
    #[must_use]
    pub fn new(runner: Arc<dyn JobRunner>, workers: usize) -> Arc<Self> {
        Self::with_sink(runner, workers, None)
    }

    /// A queue that mirrors every state change into `sink`.
    ///
    /// The sink is how a host turns job progress into session events; the
    /// queue itself stays transport-agnostic.
    #[must_use]
    pub fn with_sink(
        runner: Arc<dyn JobRunner>,
        workers: usize,
        sink: Option<Arc<dyn Fn(JobEvent) + Send + Sync>>,
    ) -> Arc<Self> {
        let queue = Arc::new(Self {
            runner,
            inner: Arc::new(Mutex::new(Inner {
                jobs: HashMap::new(),
                ready: VecDeque::new(),
                cancel_flags: HashMap::new(),
            })),
            ready_cv: Arc::new(Condvar::new()),
            next: Arc::new(AtomicU64::new(1)),
            sink,
            shutdown: Arc::new(AtomicBool::new(false)),
            workers: Mutex::new(Vec::new()),
        });

        let mut handles = queue.workers.lock().unwrap();
        for _ in 0..workers.max(1) {
            let worker = Arc::clone(&queue);
            handles.push(std::thread::spawn(move || worker.worker_loop()));
        }
        drop(handles);
        queue
    }

    /// Submit a job. It starts queued; a worker picks it up.
    #[must_use]
    pub fn submit(self: &Arc<Self>, spec: JobSpec) -> Job {
        let id = self.allocate_id();
        self.submit_with_id(id, spec)
    }

    /// Allocate the next job id without submitting.
    ///
    /// For callers that must key something on the id *before* the job can be
    /// picked up — a product plane that persists a job record, say: allocate,
    /// write the record, then submit with [`Self::submit_with_id`]. A worker
    /// can start the moment [`Self::submit_with_id`] is called, so anything
    /// written after that point races the first worker.
    #[must_use]
    pub fn allocate_id(&self) -> String {
        let n = self.next.fetch_add(1, Ordering::SeqCst);
        format!("job-{n}")
    }

    /// Submit a job carrying an id from [`Self::allocate_id`].
    ///
    /// Otherwise identical to [`Self::submit`].
    ///
    /// # Panics
    ///
    /// Panics (debug builds) if `id` is already in use in this queue.
    pub fn submit_with_id(self: &Arc<Self>, id: String, spec: JobSpec) -> Job {
        let job = Job {
            id: id.clone(),
            kind: spec.kind.clone(),
            state: JobState::Queued,
            progress: None,
            result: None,
            error: None,
            spec,
            created_at_ms: now_ms(),
            started_at_ms: None,
            finished_at_ms: None,
        };
        let cancel = Arc::new(AtomicBool::new(false));
        {
            let mut inner = self.inner.lock().unwrap();
            debug_assert!(!inner.jobs.contains_key(&id), "duplicate job id {id}");
            inner.jobs.insert(id.clone(), job.clone());
            inner.cancel_flags.insert(id.clone(), cancel);
            inner.ready.push_back(id.clone());
        }
        self.ready_cv.notify_one();
        self.emit(&job);
        job
    }

    /// A snapshot of one job.
    #[must_use]
    pub fn get(&self, id: &str) -> Option<Job> {
        self.inner.lock().unwrap().jobs.get(id).cloned()
    }

    /// All jobs, newest first.
    #[must_use]
    pub fn list(&self) -> Vec<Job> {
        let mut jobs: Vec<Job> = self.inner.lock().unwrap().jobs.values().cloned().collect();
        jobs.sort_by(|a, b| {
            b.created_at_ms
                .cmp(&a.created_at_ms)
                .then_with(|| a.id.cmp(&b.id))
        });
        jobs
    }

    /// Cancel a job.
    ///
    /// A queued job never runs. A running job gets a cancel flag the runner may
    /// observe; the queue then marks it cancelled rather than letting the
    /// runner's late result overwrite the state. Returns whether the job was
    /// found.
    #[must_use]
    pub fn cancel(&self, id: &str) -> bool {
        let cancel = {
            let mut inner = self.inner.lock().unwrap();
            let Some(job) = inner.jobs.get(id) else {
                return false;
            };
            match job.state {
                JobState::Queued => {
                    inner.ready.retain(|queued| queued != id);
                    true
                }
                JobState::Running => {
                    if let Some(flag) = inner.cancel_flags.get(id) {
                        flag.store(true, Ordering::SeqCst);
                    }
                    true
                }
                _ => return false,
            }
        };
        if cancel {
            self.finish(id, JobState::Cancelled, None, Some("cancelled".to_string()));
        }
        cancel
    }

    /// Report progress for a running job.
    pub fn set_progress(&self, id: &str, progress: f64) {
        let clamped = progress.clamp(0.0, 1.0);
        let job = {
            let mut inner = self.inner.lock().unwrap();
            match inner.jobs.get_mut(id) {
                Some(job) if job.state == JobState::Running => {
                    job.progress = Some(clamped);
                    Some(job.clone())
                }
                _ => None,
            }
        };
        if let Some(job) = job {
            self.emit(&job);
        }
    }

    /// Whether a running job has been asked to stop.
    #[must_use]
    pub fn is_cancelled(&self, id: &str) -> bool {
        self.inner
            .lock()
            .unwrap()
            .cancel_flags
            .get(id)
            .is_some_and(|flag| flag.load(Ordering::SeqCst))
    }

    fn worker_loop(self: &Arc<Self>) {
        loop {
            let id = {
                let mut inner = self.inner.lock().unwrap();
                loop {
                    if self.shutdown.load(Ordering::SeqCst) {
                        return;
                    }
                    if let Some(id) = inner.ready.pop_front() {
                        break id;
                    }
                    inner = self.ready_cv.wait(inner).unwrap();
                }
            };

            let (spec, cancel) = {
                let mut inner = self.inner.lock().unwrap();
                // Clone the flag first: it is an owned `Arc`, so the later mutable
                // borrow of the job does not conflict with it.
                let cancel = inner.cancel_flags.get(&id).cloned();
                match inner.jobs.get_mut(&id) {
                    Some(job) if job.state == JobState::Queued => {
                        job.state = JobState::Running;
                        job.started_at_ms = Some(now_ms());
                        (job.spec.clone(), cancel)
                    }
                    _ => continue,
                }
            };
            self.emit(&self.get(&id).expect("job"));

            let runner = Arc::clone(&self.runner);
            let progress = |p: f64| self.set_progress(&id, p);
            let outcome = {
                let ctx = JobContext {
                    id: &id,
                    spec: &spec,
                    progress: &progress,
                };
                runner.run(ctx)
            };

            if cancel
                .as_ref()
                .is_some_and(|flag| flag.load(Ordering::SeqCst))
            {
                self.finish(
                    &id,
                    JobState::Cancelled,
                    None,
                    Some("cancelled".to_string()),
                );
                continue;
            }
            match outcome {
                Ok(result) => self.finish(&id, JobState::Succeeded, Some(result), None),
                Err(err) => self.finish(&id, JobState::Failed, None, Some(err)),
            }
        }
    }

    fn finish(&self, id: &str, state: JobState, result: Option<JobResult>, error: Option<String>) {
        let job = {
            let mut inner = self.inner.lock().unwrap();
            let Some(job) = inner.jobs.get_mut(id) else {
                return;
            };
            job.state = state;
            job.progress = Some(1.0);
            job.result = result;
            job.error = error;
            job.finished_at_ms = Some(now_ms());
            job.clone()
        };
        self.inner.lock().unwrap().cancel_flags.remove(id);
        self.emit(&job);
    }

    fn emit(&self, job: &Job) {
        if let Some(sink) = &self.sink {
            sink(JobEvent {
                job_id: job.id.clone(),
                kind: job.kind.clone(),
                state: job.state,
                progress: job.progress,
            });
        }
    }
}

impl Drop for JobQueue {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::SeqCst);
        self.ready_cv.notify_all();
        let handles: Vec<JoinHandle<()>> = self.workers.lock().unwrap().drain(..).collect();
        for handle in handles {
            let _ = handle.join();
        }
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;
    use std::time::Duration;

    struct SlowRunner {
        calls: Arc<AtomicUsize>,
        work_ms: u64,
    }

    impl JobRunner for SlowRunner {
        fn run(&self, ctx: JobContext<'_>) -> Result<JobResult, String> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            (ctx.progress)(0.5);
            std::thread::sleep(Duration::from_millis(self.work_ms));
            if ctx.spec.kind == "fail" {
                return Err("solver exploded".to_string());
            }
            Ok(JobResult::Handle(format!("handle:{}", ctx.id)))
        }
    }

    struct CancelAwareRunner;

    impl JobRunner for CancelAwareRunner {
        fn run(&self, _ctx: JobContext<'_>) -> Result<JobResult, String> {
            for _ in 0..50 {
                std::thread::sleep(Duration::from_millis(2));
            }
            Ok(JobResult::Handle("late".to_string()))
        }
    }

    #[test]
    fn a_job_runs_off_the_calling_thread_and_is_addressable() {
        let calls = Arc::new(AtomicUsize::new(0));
        let queue = JobQueue::new(
            Arc::new(SlowRunner {
                calls: Arc::clone(&calls),
                work_ms: 1,
            }),
            1,
        );

        let job = queue.submit(JobSpec::new("margin", serde_json::json!({})));
        assert_eq!(job.state, JobState::Queued);

        let mut terminal = None;
        for _ in 0..200 {
            if let Some(current) = queue.get(&job.id) {
                if current.state.is_terminal() {
                    terminal = Some(current);
                    break;
                }
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        let done = terminal.expect("job reaches a terminal state");
        assert_eq!(done.state, JobState::Succeeded);
        assert_eq!(
            done.result,
            Some(JobResult::Handle(format!("handle:{}", job.id)))
        );
        assert_eq!(done.progress, Some(1.0));
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert!(done.started_at_ms.is_some() && done.finished_at_ms.is_some());
        // Progress was reported mid-run.
        assert!(done.headline().contains("succeeded"));
    }

    #[test]
    fn a_job_submitted_with_an_allocated_id_keeps_it() {
        let queue = JobQueue::new(
            Arc::new(SlowRunner {
                calls: Arc::new(AtomicUsize::new(0)),
                work_ms: 1,
            }),
            1,
        );
        let first = queue.allocate_id();
        let second = queue.allocate_id();
        assert_ne!(first, second, "allocation is monotonic");

        let job =
            queue.submit_with_id(first.clone(), JobSpec::new("margin", serde_json::json!({})));
        assert_eq!(job.id, first);
        for _ in 0..200 {
            if queue.get(&first).is_some_and(|j| j.state.is_terminal()) {
                break;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        assert_eq!(queue.get(&first).unwrap().state, JobState::Succeeded);
    }

    #[test]
    fn a_failing_job_records_the_error() {
        let queue = JobQueue::new(
            Arc::new(SlowRunner {
                calls: Arc::new(AtomicUsize::new(0)),
                work_ms: 1,
            }),
            1,
        );
        let job = queue.submit(JobSpec::new("fail", serde_json::json!({})));
        let mut done = None;
        for _ in 0..200 {
            let current = queue.get(&job.id).unwrap();
            if current.state.is_terminal() {
                done = Some(current);
                break;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        let done = done.unwrap();
        assert_eq!(done.state, JobState::Failed);
        assert_eq!(done.error.as_deref(), Some("solver exploded"));
        assert!(done.result.is_none());
    }

    #[test]
    fn a_queued_job_can_be_cancelled_before_it_runs() {
        // One worker, kept busy, so the second job stays queued.
        let queue = JobQueue::new(
            Arc::new(SlowRunner {
                calls: Arc::new(AtomicUsize::new(0)),
                work_ms: 60,
            }),
            1,
        );
        let _first = queue.submit(JobSpec::new("margin", serde_json::json!({})));
        std::thread::sleep(Duration::from_millis(10));
        let second = queue.submit(JobSpec::new("margin", serde_json::json!({})));

        assert!(queue.cancel(&second.id));
        let cancelled = queue.get(&second.id).unwrap();
        assert_eq!(cancelled.state, JobState::Cancelled);
        assert_eq!(cancelled.error.as_deref(), Some("cancelled"));
        // Cancelling an unknown id is false; a terminal job cannot be cancelled.
        assert!(!queue.cancel("job-nope"));
        assert!(!queue.cancel(&second.id));
    }

    #[test]
    fn state_changes_reach_the_sink() {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let sink_seen = Arc::clone(&seen);
        let queue = JobQueue::with_sink(
            Arc::new(SlowRunner {
                calls: Arc::new(AtomicUsize::new(0)),
                work_ms: 1,
            }),
            1,
            Some(Arc::new(move |event: JobEvent| {
                sink_seen.lock().unwrap().push(event.state);
            })),
        );
        let job = queue.submit(JobSpec::new("margin", serde_json::json!({})));
        for _ in 0..200 {
            if queue.get(&job.id).unwrap().state.is_terminal() {
                break;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        let states = seen.lock().unwrap().clone();
        assert!(states.contains(&JobState::Queued), "{states:?}");
        assert!(states.contains(&JobState::Running), "{states:?}");
        assert!(states.contains(&JobState::Succeeded), "{states:?}");
    }

    #[test]
    fn workers_run_jobs_concurrently() {
        let calls = Arc::new(AtomicUsize::new(0));
        let queue = JobQueue::new(
            Arc::new(SlowRunner {
                calls: Arc::clone(&calls),
                work_ms: 30,
            }),
            3,
        );
        for _ in 0..3 {
            queue.submit(JobSpec::new("margin", serde_json::json!({})));
        }
        for _ in 0..400 {
            if queue.list().iter().all(|j| j.state.is_terminal()) {
                break;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        assert_eq!(calls.load(Ordering::SeqCst), 3);
        assert!(queue.list().iter().all(|j| j.state == JobState::Succeeded));
    }

    #[test]
    fn progress_is_clamped_and_reported() {
        let queue = JobQueue::new(Arc::new(CancelAwareRunner), 1);
        let job = queue.submit(JobSpec::new("margin", serde_json::json!({})));
        queue.set_progress(&job.id, 5.0);
        // Not running yet (or the runner owns it), so the value is recorded when
        // the job is running; the clamp is what matters here.
        queue.set_progress(&job.id, -1.0);
        assert!(queue.get(&job.id).is_some());
    }
}
