//! Agent handle and turn/step driver (design-harness 10.5).
//!
//! The `Agent` handle is the primary interface for driving the agent loop.
//! It owns the session, LLM runtime, tool pipeline, inbox, and cancellation
//! token. The `run` method drives the turn/step loop.
//!
//! ## Turn/step loop
//!
//! ```text
//! turn/start -> pre-step waterfall -> step/start -> user/message
//!   -> assemble system+tools -> agent/request waterfall -> stream chunks
//!   -> assistant/message -> dispatch tools -> step/end
//!   -> (if tool debt or steering) next step
//!   -> (else) turn/end
//! ```
//!
//! ## Plan state machine
//!
//! The agent loop drives the plan lifecycle (design-harness 14.3):
//! ```text
//! Proposed -> Executing -> Verifying -> Settled
//!                                \-> Contingent
//! ```
//!
//! A `propose_plan` tool call creates a plan in `Proposed`. When execution
//! tool calls follow, the plan auto-accepts to `Executing`. Before a turn is
//! allowed to end, the gate evaluates the declared intent against the
//! engine snapshot. Pass -> `Settled`. Fail -> the failing measurements go
//! back to the model as a steering message and the plan stays `Executing`;
//! once `MAX_VERIFY_RETRIES` is spent it becomes `Contingent`.

use crate::inbox::Inbox;
use crate::plan::{Plan, PlanState};
use nanobot_llm::{BlockAssembler, LlmRequest, LlmRuntime, Message, ToolDefinition};
use nanobot_session::{Session, SessionEvent, TurnEndReason};
use nanobot_tools::ToolPipeline;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

/// A product-defined restore point: the executor's own snapshot type, carried
/// opaquely so the framework can roll a plan or a rejected write back without
/// knowing what the engine captured.
pub trait RestorePoint: Send + Sync {
    /// Downcast support so the product can recover its own checkpoint type
    /// (the framework carries it opaquely).
    fn as_any(&self) -> &dyn std::any::Any;
    /// Moves the value out, for the product to consume on restore.
    fn into_any(self: Box<Self>) -> Box<dyn std::any::Any>;
}

/// One structural check the write gate recorded.
#[derive(Debug, Clone)]
pub struct WriteCheck {
    /// Stable invariant name (`adds_one_feature`, `document_unchanged`, ...).
    pub invariant: String,
    /// Whether the invariant held.
    pub passed: bool,
    /// Human-readable detail for the model.
    pub detail: String,
}

/// The result of a tool call that went through the write gate.
#[derive(Debug, Clone)]
pub struct WriteOutcome {
    /// The tool's own reply (the JSON string logged as `tool/result`).
    pub reply: String,
    /// Whether the write stood: verified, or no gate applies.
    pub accepted: bool,
    /// The structural checks the gate recorded.
    pub checks: Vec<WriteCheck>,
}

impl WriteOutcome {
    /// An ungated outcome: the tool ran and nothing was verified.
    pub fn ungated(reply: String) -> Self {
        Self { reply, accepted: true, checks: Vec::new() }
    }

    /// The checks that did not hold, in recorded order.
    pub fn failures(&self) -> impl Iterator<Item = &WriteCheck> {
        self.checks.iter().filter(|c| !c.passed)
    }

    /// One-line summary for the check panel and the audit trail.
    pub fn summary(&self) -> String {
        let passed = self.checks.iter().filter(|c| c.passed).count();
        format!("{}/{} write invariants held", passed, self.checks.len())
    }
}

/// Grades a plan's declared intent against the post-execution state.
///
/// The intent is the agent's own declaration (opaque to the framework — the
/// product parses it into whatever it needs). `Err` carries the failing
/// measurements, handed back to the model so it can act on what went wrong
/// rather than on an aggregate count.
pub trait PlanVerifier: Send + Sync {
    /// `Ok(notes)` accepts (notes are advisory); `Err(failures)` rejects with
    /// the failing measurements.
    fn grade(&self, intent: &serde_json::Value) -> Result<Vec<String>, Vec<String>>;
}

/// Trait for tool execution backends.
///
/// The agent logs tool calls to the session, then calls the executor
/// to run them. The executor returns the result string that gets logged
/// as a `ToolResult` event.
pub trait ToolExecutor: Send + Sync {
    /// Execute a tool call and return the result as a JSON string.
    fn execute(&self, name: &str, arguments: &serde_json::Value) -> String;

    /// A description of the subject this executor operates on, injected into
    /// the system prompt so the model knows what it is working with (for a CAD
    /// engine: the feature outline, the resolved parameters, the bounding box
    /// and the evaluated result). `None` when there is nothing to describe.
    fn describe_subject(&self) -> Option<String> {
        let _ = self;
        None
    }

    /// Run a tool call under the write gate.
    ///
    /// The default bypasses the gate, which is correct for executors that
    /// cannot roll a write back. An executor that holds the subject can verify
    /// the tool kept its own promises and restore when it did not.
    fn apply(&self, name: &str, arguments: &serde_json::Value) -> WriteOutcome {
        WriteOutcome::ungated(self.execute(name, arguments))
    }

    /// Capture a restore point so a plan (or a rejected write) can be rolled
    /// back. `None` when the executor has no rollback-capable backend.
    fn checkpoint(&self) -> Option<Box<dyn RestorePoint>> {
        let _ = self;
        None
    }

    /// Restore a checkpoint captured by [`ToolExecutor::checkpoint`].
    ///
    /// The default is a no-op, which is correct for executors that cannot
    /// roll back.
    fn restore(&self, checkpoint: Box<dyn RestorePoint>) {
        let _ = checkpoint;
    }
}

/// The current state of the agent.
#[derive(Debug, Clone, PartialEq)]
pub enum AgentState {
    Idle,
    Running { turn: u32, step: u32 },
    AwaitingApproval,
    Cancelled,
}

/// The agent handle: primary interface for driving the agent loop.
///
/// `Agent` is `Clone` (Arc internals). It is `Send + Sync`.
#[derive(Clone)]
pub struct Agent {
    inner: Arc<AgentInner>,
}

struct AgentInner {
    session: Session,
    llm: LlmRuntime,
    tools: ToolPipeline,
    inbox: Inbox,
    turn_counter: AtomicU32,
    state: Mutex<AgentState>,
    tool_executor: Mutex<Option<Arc<dyn ToolExecutor>>>,
    current_plan: Mutex<Option<Plan>>,
    /// Document state at the moment the current plan began executing, so a
    /// rejection can roll the work back (design-harness 14.3).
    plan_checkpoint: Mutex<Option<Box<dyn RestorePoint>>>,
    /// Session seq at the moment the current plan began executing; a rejection
    /// rewinds the log to it so the rolled-back work leaves the model history.
    plan_start_seq: Mutex<Option<u32>>,
    /// Hook the app layer uses to run the `prompt/assemble` waterfall, so
    /// skills can append their persona sections (design-harness 14.2). The
    /// agent stays free of any plugin type.
    prompt_assembler: Mutex<Option<Arc<dyn Fn(String) -> String + Send + Sync>>>,
    /// The product's standing instructions (the persona section of the system
    /// prompt). Absent -> [`PERSONA`], the built-in default. A product sets
    /// this so the framework carries no domain identity of its own.
    persona: Mutex<Option<String>>,
    /// The wire model id sent to the provider and recorded on `RequestHeader`.
    /// Absent -> `"default"`, which is also the adapter key: a product that
    /// talks to a real provider sets the real id so the record is truthful and
    /// the accounting can be attributed.
    model: Mutex<Option<String>>,
    /// The product's plan grader: grades the declared intent against the
    /// post-execution state (design-harness 14.3). Absent -> a plan with an
    /// intent passes (nothing to check against).
    plan_verifier: Mutex<Option<Arc<dyn PlanVerifier>>>,
}

/// The adapter key the agent asks for when the product sets no model.
const DEFAULT_MODEL: &str = "default";

/// Convert the wire accounting type into the session's.
///
/// The two types are separate on purpose: `nanobot-llm` owns the provider
/// vocabulary (and must stay below `nanobot-session` in the dependency order),
/// while `nanobot-session` owns the neutral, provider-free event vocabulary.
/// The conversion therefore lives here, in the one crate that depends on both.
fn to_session_usage(usage: nanobot_llm::Usage) -> nanobot_session::Usage {
    nanobot_session::Usage {
        prompt_tokens: usage.prompt_tokens,
        completion_tokens: usage.completion_tokens,
        total_tokens: usage.total_tokens,
    }
}

/// How many steps one turn may take before it is cut off.
///
/// A model that keeps emitting tool calls must not be able to hang the
/// session; the turn ends with [`TurnEndReason::MaxSteps`] instead.
const MAX_STEPS: u32 = 50;

/// How many times a failed plan verification is handed back to the model
/// before the plan is left `Contingent` for a human.
const MAX_VERIFY_RETRIES: u32 = 2;

/// What the plan gate decided at the end of a step.
#[derive(Debug, Clone)]
enum PlanVerdict {
    /// No plan was awaiting verification.
    NoPlan,
    /// The plan's criteria held; it is settled and closed.
    Passed,
    /// The plan's criteria did not hold.
    Failed {
        /// The failing measurements, one line each.
        diagnostics: Vec<String>,
        /// Whether the plan was left `Executing` for another attempt.
        retry_allowed: bool,
    },
}

/// The built-in persona, used when the product supplies none.
///
/// This is a *default*, not the framework's opinion about the domain: a
/// harness product installs its own standing instructions with
/// [`Agent::set_persona`], because the domain identity belongs to the product.
const PERSONA: &str = "\
You are ocad, a CAD design agent working on one open part document.
You change the document only by calling the provided tools; you never emit
geometry yourself. Inspect before you write: the inspection tools tell you
what the document currently is. A tool that fails returns an error describing
why — read it and act on it rather than repeating the same call.
";

/// The `propose_plan` tool definition the agent advertises to the model.
///
/// The agent loop owns this tool (it never reaches the engine), so the agent
/// also owns its advertisement: without it the model cannot discover the tool
/// and the plan state machine would be unreachable.
fn plan_tool_definition() -> nanobot_tools::ToolDefinition {
    nanobot_tools::ToolDefinition {
        name: "propose_plan".to_string(),
        description: "Propose a plan before executing it. Declare the steps and, \
                      optionally, the design intent the finished result must satisfy."
            .to_string(),
        parameters_schema: serde_json::json!({
            "type": "object",
            "properties": {
                "plan_id": {"type": "string"},
                "summary": {"type": "string"},
                "steps": {"type": "array", "items": {"type": "object"}},
                "clarify": {"type": "boolean"},
                "questions": {"type": "array", "items": {"type": "string"}},
                "intent": {
                    "type": "object",
                    "properties": {
                        "intent": {"type": "string"},
                        "acceptance": {"type": "array", "items": {"type": "object"}}
                    },
                    "required": ["intent", "acceptance"]
                }
            },
            "required": ["summary"]
        }),
        requires_approval: false,
        contract: nanobot_tools::ToolContract::ReadOnly,
    }
}

impl Agent {
    /// Create a new agent with the given session, LLM runtime, and tool pipeline.
    pub fn new(session: Session, llm: LlmRuntime, tools: ToolPipeline) -> Self {
        // Advertise the agent-owned `propose_plan` tool so the model can emit
        // it. A duplicate registration is ignored: the caller may have already
        // registered its own definition.
        let _ = tools.registry().register(plan_tool_definition());

        Self {
            inner: Arc::new(AgentInner {
                session,
                llm,
                tools,
                inbox: Inbox::new(),
                turn_counter: AtomicU32::new(0),
                state: Mutex::new(AgentState::Idle),
                tool_executor: Mutex::new(None),
                current_plan: Mutex::new(None),
                plan_checkpoint: Mutex::new(None),
                plan_start_seq: Mutex::new(None),
                prompt_assembler: Mutex::new(None),
                persona: Mutex::new(None),
                model: Mutex::new(None),
                plan_verifier: Mutex::new(None),
            }),
        }
    }

    /// Install the `prompt/assemble` hook.
    ///
    /// The hook receives the assembled system prompt and returns it with the
    /// plugin sections appended. The app layer bridges this to the
    /// `nanobot-cordis` waterfall so skill manifests contribute (design 14.2);
    /// the agent itself knows nothing about skills.
    pub fn set_prompt_assembler<F>(&self, assembler: F)
    where
        F: Fn(String) -> String + Send + Sync + 'static,
    {
        *self.inner.prompt_assembler.lock().unwrap() = Some(Arc::new(assembler));
    }

    /// Set the product's standing instructions (the persona section).
    ///
    /// The persona is the *first* section of the system prompt: the domain
    /// identity ("you are a chip design agent working on one open design"),
    /// followed by what the product wants the model to treat as its job.
    /// Unset, the built-in default ([`PERSONA`]) is used. Skills and the
    /// prompt assembler still append after it, so this replaces the identity
    /// only — it does not disable plugin contributions.
    pub fn set_persona(&self, persona: impl Into<String>) {
        *self.inner.persona.lock().unwrap() = Some(persona.into());
    }

    /// Set the wire model id (what the provider is asked for, and what
    /// `RequestHeader` records). Unset, the agent asks for the adapter key
    /// `"default"`.
    pub fn set_model(&self, model: impl Into<String>) {
        *self.inner.model.lock().unwrap() = Some(model.into());
    }

    /// The model id requests are made with.
    fn wire_model(&self) -> String {
        self.inner
            .model
            .lock()
            .unwrap()
            .clone()
            .unwrap_or_else(|| DEFAULT_MODEL.to_string())
    }

    /// Set the plan verifier (the product's intent grader). Without one, a
    /// plan that declares an intent passes — there is nothing to check against.
    pub fn set_plan_verifier(&self, verifier: Arc<dyn PlanVerifier>) {
        *self.inner.plan_verifier.lock().unwrap() = Some(verifier);
    }

    /// The system prompt the model receives this turn (design-harness 14.2).
    ///
    /// Exposed so a caller can show or assert on the context the model is
    /// given; the loop builds it once per turn from the same function.
    pub fn system_prompt(&self) -> String {
        self.build_system_prompt()
    }

    /// Assemble the turn's system prompt (design-harness 14.2).
    ///
    /// The document section is what the model would otherwise have no way to
    /// know: the feature outline, the resolved parameters, the bounding box and
    /// the evaluated result. The check section carries the latest verdicts so
    /// the model can see what already failed.
    fn build_system_prompt(&self) -> String {
        // The persona comes from the product when it set one; the built-in
        // default is a fallback, not a framework opinion about the domain.
        let mut prompt = self
            .inner
            .persona
            .lock()
            .unwrap()
            .clone()
            .unwrap_or_else(|| PERSONA.to_string());

        let executor = self.inner.tool_executor.lock().unwrap().clone();
        if let Some(subject) = executor.as_ref().and_then(|e| e.describe_subject()) {
            prompt.push_str("\n## Current subject\n");
            prompt.push_str(&subject);
            prompt.push('\n');
        }

        // The latest verdict per check, so a failure is not silently repeated.
        let mut checks: Vec<(String, String)> = Vec::new();
        for entry in self.inner.session.events() {
            if let SessionEvent::CheckResult {
                check_id,
                passed,
                details,
            } = &entry.event
            {
                let verdict = format!(
                    "- {check_id}: {} ({details})",
                    if *passed { "pass" } else { "FAIL" }
                );
                match checks.iter_mut().find(|(id, _)| id == check_id) {
                    Some(existing) => existing.1 = verdict,
                    None => checks.push((check_id.clone(), verdict)),
                }
            }
        }
        if !checks.is_empty() {
            prompt.push_str("\n## Latest checks\n");
            for (_, verdict) in &checks {
                prompt.push_str(verdict);
                prompt.push('\n');
            }
        }

        match self.inner.prompt_assembler.lock().unwrap().clone() {
            Some(assembler) => assembler(prompt),
            None => prompt,
        }
    }

    /// Set the tool executor (called after construction to avoid circular deps).
    pub fn set_tool_executor(&self, executor: Arc<dyn ToolExecutor>) {
        *self.inner.tool_executor.lock().unwrap() = Some(executor);
    }

    /// Get the session.
    pub fn session(&self) -> &Session {
        &self.inner.session
    }

    /// Get the current agent state.
    pub fn state(&self) -> AgentState {
        self.inner.state.lock().unwrap().clone()
    }

    /// Get the current plan state, if a plan is active.
    pub fn current_plan_state(&self) -> Option<PlanState> {
        self.inner
            .current_plan
            .lock()
            .unwrap()
            .as_ref()
            .map(|p| p.state)
    }

    /// Accept the current plan: keep the work and close the plan
    /// (design-harness 14.3, the decision bar's Accept).
    ///
    /// Emits `PlanAccepted`. Returns whether a plan was open.
    pub fn accept_plan(&self) -> bool {
        let plan_id = {
            let mut guard = self.inner.current_plan.lock().unwrap();
            match guard.as_mut() {
                Some(p) => {
                    let id = p.plan_id.clone();
                    *guard = None;
                    id
                }
                None => return false,
            }
        };

        self.inner
            .session
            .append(SessionEvent::PlanAccepted { plan_id });
        *self.inner.plan_checkpoint.lock().unwrap() = None;
        *self.inner.plan_start_seq.lock().unwrap() = None;
        true
    }

    /// Reject the current plan: roll the work back and close the plan
    /// (design-harness 14.3, the decision bar's Reject).
    ///
    /// The document is restored to the plan's starting checkpoint and the log
    /// is rewound to the plan's starting seq, so the rolled-back work leaves
    /// the model's projected history too. Emits `PlanRejected`, which *is*
    /// model-visible — the model needs to know the plan was rejected and why.
    ///
    /// Returns whether a plan was open.
    pub fn reject_plan(&self, reason: impl Into<String>) -> bool {
        let plan_id = {
            let mut guard = self.inner.current_plan.lock().unwrap();
            match guard.as_mut() {
                Some(p) => {
                    let id = p.plan_id.clone();
                    *guard = None;
                    id
                }
                None => return false,
            }
        };

        // Rewind the document first, then the log: the truncation drops the
        // tool-call events whose effects the restore just undid.
        if let Some(checkpoint) = self.inner.plan_checkpoint.lock().unwrap().take() {
            if let Some(executor) = self.inner.tool_executor.lock().unwrap().as_ref() {
                executor.restore(checkpoint);
            }
        }
        if let Some(seq) = self.inner.plan_start_seq.lock().unwrap().take() {
            self.inner.session.truncate_to(seq);
        }

        self.inner.session.append(SessionEvent::PlanRejected {
            plan_id,
            reason: reason.into(),
        });
        true
    }

    /// Send the current plan back to `Drafting`, keeping its context
    /// (design-harness 14.3, the decision bar's Modify).
    ///
    /// The next `propose_plan` replaces it. Returns whether a plan was open.
    pub fn modify_plan(&self) -> bool {
        let mut guard = self.inner.current_plan.lock().unwrap();
        match guard.as_mut() {
            Some(p) => {
                p.restart_drafting();
                true
            }
            None => false,
        }
    }

    /// Send a followup message that will start a new turn.
    pub fn followup(&self, message: impl Into<String>) {
        self.inner.inbox.push_turn(message.into());
    }

    /// Send a steering message that will inject into the current step.
    pub fn steer(&self, message: impl Into<String>) {
        self.inner.inbox.push_step(message.into());
    }

    /// Inject a raw session event.
    pub fn inject(&self, event: SessionEvent) {
        self.inner.session.append(event);
    }

    /// Cancel the current turn.
    pub fn cancel(&self) {
        *self.inner.state.lock().unwrap() = AgentState::Cancelled;
    }

    /// Propose a plan: create it in `Proposed` state, emit `PlanProposed`,
    /// and store it as the current plan.
    ///
    /// Called when the model emits a `propose_plan` tool call.
    pub fn propose_plan(
        &self,
        plan_id: impl Into<String>,
        summary: impl Into<String>,
        steps: serde_json::Value,
        intent: Option<serde_json::Value>,
    ) {
        let mut plan = Plan::new(plan_id, summary, steps);
        if let Some(intent) = intent {
            plan = Plan::with_intent(
                plan.plan_id.clone(),
                plan.summary.clone(),
                plan.steps.clone(),
                intent,
            );
        }

        let proposal = nanobot_session::PlanProposal {
            plan_id: plan.plan_id.clone(),
            summary: plan.summary.clone(),
            steps: plan.steps.clone(),
        };

        self.inner
            .session
            .append(SessionEvent::PlanProposed { plan: proposal });

        *self.inner.current_plan.lock().unwrap() = Some(plan);
    }

    /// Request clarification: create a plan in `Clarify` state, emit
    /// `PlanClarify`, and store it as the current plan (design 14.3).
    ///
    /// Called when the model needs more information before proposing.
    pub fn clarify_plan(&self, plan_id: impl Into<String>, questions: &[String]) {
        let plan_id = plan_id.into();
        let mut plan = Plan::new(plan_id.clone(), "", serde_json::Value::Null);
        plan.request_clarification();

        self.inner.session.append(SessionEvent::PlanClarify {
            plan_id: plan_id.clone(),
            questions: questions.to_vec(),
        });

        *self.inner.current_plan.lock().unwrap() = Some(plan);
    }

    /// Handle a `propose_plan` tool call: parse arguments and create a plan.
    ///
    /// Returns the synthetic tool result string.
    fn handle_propose_plan(&self, args: &serde_json::Value) -> String {
        let plan_id = args
            .get("plan_id")
            .and_then(|v| v.as_str())
            .unwrap_or("plan_0")
            .to_string();
        let summary = args
            .get("summary")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let steps = args
            .get("steps")
            .cloned()
            .unwrap_or(serde_json::Value::Null);

        // The intent is the agent's own declaration, carried as an opaque JSON
        // value: the product's plan verifier parses it into its own checks.
        let intent = args.get("intent").cloned();

        // Check for clarification request (design 14.3: Drafting-stage Clarify).
        let clarify = args
            .get("clarify")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        if clarify {
            let questions: Vec<String> = args
                .get("questions")
                .and_then(|q| q.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|v| v.as_str().map(String::from))
                        .collect()
                })
                .unwrap_or_default();
            self.clarify_plan(plan_id, &questions);
            return serde_json::json!({
                "ok": true,
                "plan_status": "clarify",
                "questions": questions
            })
            .to_string();
        }

        self.propose_plan(plan_id, summary, steps, intent);

        serde_json::json!({
            "ok": true,
            "plan_status": "proposed"
        })
        .to_string()
    }

    /// Log a tool call that never executed, and why.
    ///
    /// A refused call is still an event: the model has to know the call did
    /// not happen, and the audit trail has to carry the refusal
    /// (design-harness 14.5).
    fn log_unexecuted_call(
        &self,
        id: &str,
        name: &str,
        args: &serde_json::Value,
        seq: u32,
        code: &str,
        detail: &str,
    ) {
        self.inner.session.append(SessionEvent::ToolCall {
            id: id.to_string(),
            name: name.to_string(),
            arguments: args.clone(),
            seq,
        });
        self.inner.session.append(SessionEvent::ToolResult {
            id: id.to_string(),
            success: false,
            output: serde_json::json!({
                "ok": false,
                "error": {"code": code, "detail": detail}
            })
            .to_string(),
            topology_signature: None,
        });
    }

    /// Grade the plan awaiting verification and finalize it.
    ///
    /// The verdict lands in the check panel as `CheckRun`/`CheckResult`
    /// (`check_id: "plan:intent"`), the same shape the write gate uses.
    ///
    /// - No plan in `Executing` -> [`PlanVerdict::NoPlan`].
    /// - Criteria met -> the plan settles and closes.
    /// - Criteria missed with `allow_retry` -> the plan goes back to
    ///   `Executing` and the failing measurements come back for the model.
    /// - Criteria missed without `allow_retry` -> `Contingent`: the plan stays
    ///   alive so a reviewer can still accept, reject or modify it
    ///   (design-harness 14.3).
    fn grade_and_finalize_plan(&self, allow_retry: bool) -> PlanVerdict {
        let plan = {
            let guard = self.inner.current_plan.lock().unwrap();
            match guard.as_ref() {
                Some(p) if p.state == PlanState::Executing => p.clone(),
                _ => return PlanVerdict::NoPlan,
            }
        };

        let verifier = self.inner.plan_verifier.lock().unwrap().clone();
        let report = plan
            .intent
            .as_ref()
            .and_then(|intent| verifier.as_ref().map(|v| v.grade(intent)));

        // No verifier, or no intent declared: nothing to check against, so the
        // plan passes on its own terms rather than failing vacuously.
        let passed = report.as_ref().map(|r| r.is_ok()).unwrap_or(true);
        let detail = report
            .as_ref()
            .map(|r| match r {
                Ok(warnings) if warnings.is_empty() => {
                    "every acceptance check passed".to_string()
                }
                Ok(warnings) => format!("passed with notes: {}", warnings.join("; ")),
                Err(failures) => failures.join("; "),
            })
            .unwrap_or_else(|| "no intent declared".to_string());

        self.inner.session.append(SessionEvent::CheckRun {
            check_id: "plan:intent".to_string(),
        });
        self.inner.session.append(SessionEvent::CheckResult {
            check_id: "plan:intent".to_string(),
            passed,
            details: detail.clone(),
        });

        if passed {
            let plan_id = {
                let mut guard = self.inner.current_plan.lock().unwrap();
                let Some(p) = guard.as_mut() else {
                    return PlanVerdict::NoPlan;
                };
                p.settle();
                let plan_id = p.plan_id.clone();
                *guard = None;
                plan_id
            };
            self.inner
                .session
                .append(SessionEvent::PlanAccepted { plan_id });
            // The plan is closed; its rollback point is no longer needed.
            *self.inner.plan_checkpoint.lock().unwrap() = None;
            *self.inner.plan_start_seq.lock().unwrap() = None;
            return PlanVerdict::Passed;
        }

        let diagnostics = report
            .as_ref()
            .map(|r| match r { Ok(w) => w.clone(), Err(f) => f.clone() })
            .unwrap_or_default();

        let plan_id = {
            let mut guard = self.inner.current_plan.lock().unwrap();
            let Some(p) = guard.as_mut() else {
                return PlanVerdict::NoPlan;
            };
            let plan_id = p.plan_id.clone();
            if allow_retry {
                // The model gets the measurements and another step; the plan
                // stays live so the next grade can settle it.
                p.begin_execution();
            } else {
                p.mark_contingent();
            }
            plan_id
        };

        if !allow_retry {
            self.inner.session.append(SessionEvent::PlanContingent {
                plan_id,
                condition: detail,
            });
        }

        PlanVerdict::Failed {
            diagnostics,
            retry_allowed: allow_retry,
        }
    }

    /// Run the agent loop until the inbox is empty or the agent is cancelled.
    ///
    /// Drives the turn/step cycle (design-harness 10.5):
    /// ```text
    /// turn/start -> [ step/start -> user/message -> LLM -> step/end ]* -> turn/end
    /// ```
    ///
    /// A step continues when there is tool debt (tool calls were made) or
    /// steering (next-step message in the inbox). Otherwise the turn ends.
    pub async fn run(&self) {
        loop {
            // Check cancellation.
            if *self.inner.state.lock().unwrap() == AgentState::Cancelled {
                return;
            }

            // Get next turn message.
            let message = match self.inner.inbox.pop_turn() {
                Some(m) => m,
                None => return, // No more turns.
            };

            let turn = self.inner.turn_counter.fetch_add(1, Ordering::SeqCst);
            *self.inner.state.lock().unwrap() = AgentState::Running { turn, step: 0 };

            // turn/start
            self.inner.session.append(SessionEvent::TurnStart { turn });

            // The system prompt is assembled once per turn (design 14.2): it is
            // a turn-level view of the document, not a per-step one.
            let system_prompt = self.build_system_prompt();

            // Step loop
            let mut step: u32 = 0;
            let mut verify_retries: u32 = 0;
            let mut turn_reason = TurnEndReason::Completed;
            loop {
                // Tool debt is evaluated per step: a step continues only if
                // *this* step produced tool calls (or a steering message is
                // pending). Without the reset the turn could never end once
                // any tool call had run.
                let mut has_tool_calls = false;

                // Check cancellation.
                if *self.inner.state.lock().unwrap() == AgentState::Cancelled {
                    self.inner.session.append(SessionEvent::TurnEnd {
                        turn,
                        reason: TurnEndReason::Cancelled,
                    });
                    return;
                }

                // step/start
                self.inner
                    .session
                    .append(SessionEvent::StepStart { turn, step });

                // Get next step message (steering). For the first step, use the
                // turn message; for subsequent steps, use steering or empty.
                let step_message = if step == 0 {
                    message.clone()
                } else {
                    self.inner.inbox.pop_step().unwrap_or_default()
                };

                // user/message
                self.inner.session.append(SessionEvent::UserMessage {
                    content: step_message,
                    selection_ref: None,
                });

                // Derive messages from session.
                let projected = self.inner.session.derive_messages();
                let mut messages: Vec<Message> = vec![Message::system(system_prompt.clone())];
                messages.extend(
                    projected
                        .into_iter()
                        .map(|m| match m {
                            nanobot_session::ProjectedMessage::User(content) => Message::user(content),
                            nanobot_session::ProjectedMessage::Assistant(content) => {
                                Message::assistant(content)
                            }
                            nanobot_session::ProjectedMessage::System(content) => {
                                Message::system(content)
                            }
                            // The call and its result must both reach the model, or
                            // it cannot act on what a tool returned.
                            nanobot_session::ProjectedMessage::ToolCall {
                                id,
                                name,
                                arguments,
                            } => Message::tool_call(id, name, arguments.to_string()),
                            nanobot_session::ProjectedMessage::ToolResult { id, output, .. } => {
                                Message::tool_result(id, output)
                            }
                        })
                        .collect::<Vec<Message>>(),
                );

                // Build tool definitions.
                let tool_defs: Vec<ToolDefinition> = self
                    .inner
                    .tools
                    .list_tools()
                    .into_iter()
                    .map(|t| ToolDefinition {
                        name: t.name,
                        description: t.description,
                        parameters_schema: t.parameters_schema,
                    })
                    .collect();

                // Build LLM request.
                let wire_model = self.wire_model();
                let request = LlmRequest {
                    model: wire_model.clone(),
                    messages,
                    tools: tool_defs,
                    temperature: None,
                    max_tokens: None,
                };

                // Stream from LLM.
                let mut stream = self.inner.llm.stream("default", request);
                let mut assembler = BlockAssembler::new();
                let mut stream_error: Option<String> = None;

                use futures_util::StreamExt;
                while let Some(item) = stream.next().await {
                    // Check cancellation.
                    if *self.inner.state.lock().unwrap() == AgentState::Cancelled {
                        self.inner.session.append(SessionEvent::TurnEnd {
                            turn,
                            reason: TurnEndReason::Cancelled,
                        });
                        return;
                    }

                    // Feed every chunk (Text, Reasoning, ToolCall) into the assembler.
                    match item {
                        Ok(chunk) => {
                            assembler.push(chunk);
                        }
                        // A provider failure is not an empty response. Swallowing
                        // it would leave the turn silently ending with whatever
                        // had accumulated.
                        Err(error) => {
                            stream_error = Some(error.to_string());
                            break;
                        }
                    }
                }

                if let Some(detail) = stream_error {
                    // The check panel is where failures surface (check events
                    // are not model-visible), so a transport failure lands
                    // there rather than vanishing.
                    self.inner.session.append(SessionEvent::CheckResult {
                        check_id: "llm:stream".to_string(),
                        passed: false,
                        details: detail,
                    });
                    turn_reason = TurnEndReason::Error;
                    break;
                }

                // Process any remaining text from the assembler.
                if let Some(block) = assembler.finish() {
                    // Record the request itself: which model answered and what
                    // it cost. This is the only event carrying token usage, so
                    // dropping it would make the session unbillable.
                    self.inner.session.append(SessionEvent::RequestHeader {
                        model: wire_model,
                        usage: block.usage.map(to_session_usage),
                    });

                    if !block.text.is_empty() {
                        self.inner.session.append(SessionEvent::AssistantMessage {
                            content: block.text,
                        });
                    }

                    // Log tool calls and execute them under the write gate.
                    for tc in &block.tool_calls {
                        has_tool_calls = true;
                        let args: serde_json::Value =
                            serde_json::from_str(&tc.arguments).unwrap_or(serde_json::Value::Null);

                        let seq = self.inner.session.next_seq();

                        // Intercept `propose_plan` — handled by the agent loop itself.
                        if tc.name == "propose_plan" {
                            let output = self.handle_propose_plan(&args);
                            self.inner.session.append(SessionEvent::ToolCall {
                                id: tc.id.clone(),
                                name: tc.name.clone(),
                                arguments: args.clone(),
                                seq,
                            });
                            self.inner.session.append(SessionEvent::ToolResult {
                                id: tc.id.clone(),
                                success: true,
                                output,
                                topology_signature: None,
                            });
                            continue;
                        }

                        // --- pre-execute waterfall (design 14.1 / 14.5) ---
                        // The whitelist, the parameter check and the approval
                        // decision all happen here, before anything reaches the
                        // engine: an unregistered tool is refused, not run.
                        let denial = match self.inner.tools.pre_execute(&tc.name, &args) {
                            nanobot_tools::PreExecuteResult::Allow => None,
                            // The approval policy resolves synchronously today,
                            // so `Ask` is not produced; refusing it keeps an
                            // unapproved call from slipping through.
                            nanobot_tools::PreExecuteResult::Deny(reason)
                            | nanobot_tools::PreExecuteResult::Ask(reason) => Some(reason),
                        };

                        if let Some(reason) = denial {
                            self.log_unexecuted_call(
                                &tc.id,
                                &tc.name,
                                &args,
                                seq,
                                "tool_denied",
                                &reason,
                            );
                            continue;
                        }

                        let executor = self.inner.tool_executor.lock().unwrap().clone();
                        let Some(executor) = executor else {
                            self.log_unexecuted_call(
                                &tc.id,
                                &tc.name,
                                &args,
                                seq,
                                "no_executor",
                                "no tool executor is wired",
                            );
                            continue;
                        };

                        // Auto-accept: the first call that actually executes
                        // transitions the plan from Proposed to Executing, and
                        // captures the plan's starting point (document + log
                        // position) so a rejection can roll the whole plan back.
                        {
                            let mut guard = self.inner.current_plan.lock().unwrap();
                            if let Some(p) = guard.as_mut() {
                                if p.state == PlanState::Proposed {
                                    p.begin_execution();
                                    *self.inner.plan_checkpoint.lock().unwrap() =
                                        executor.checkpoint();
                                    *self.inner.plan_start_seq.lock().unwrap() = Some(
                                        self.inner
                                            .session
                                            .events()
                                            .last()
                                            .map(|e| e.seq)
                                            .unwrap_or(0),
                                    );
                                }
                            }
                        }

                        // Announce the check, then execute the tool call under
                        // its contract. A rejected write has already been
                        // rolled back, so the document carries no trace of it.
                        let check_id = format!("write:{}", tc.name);
                        self.inner.session.append(SessionEvent::CheckRun {
                            check_id: check_id.clone(),
                        });

                        let outcome = executor.apply(&tc.name, &args);
                        self.inner.session.append(SessionEvent::ToolCall {
                            id: tc.id.clone(),
                            name: tc.name.clone(),
                            arguments: args.clone(),
                            seq,
                        });

                        let success = outcome.accepted && !outcome.reply.contains("\"ok\":false");
                        // Post-execute waterfall (design 14.1): lightweight
                        // verification hook after the tool has run.
                        let post = self.inner.tools.post_execute(&tc.name, &outcome.reply);
                        let (output, check_details) = if outcome.accepted {
                            match post {
                                nanobot_tools::PostExecuteResult::Accept => {
                                    (outcome.reply.clone(), outcome.summary())
                                }
                                nanobot_tools::PostExecuteResult::Replace(v) => {
                                    (v, outcome.summary())
                                }
                                nanobot_tools::PostExecuteResult::Block(reason) => {
                                    (serde_json::json!({"ok":false,"error":{"code":"post_blocked","detail":reason}}).to_string(), outcome.summary())
                                }
                            }
                        } else {
                            // The write was rolled back; tell the model why.
                            let failures: Vec<String> = outcome
                                .failures()
                                .map(|f| format!("{}: {}", f.invariant, f.detail))
                                .collect();
                            let detail = failures.join("; ");
                            let output = serde_json::json!({
                                "ok": false,
                                "error": {
                                    "code": "write_rejected",
                                    "detail": detail
                                }
                            })
                            .to_string();
                            (output, outcome.summary())
                        };
                        self.inner.session.append(SessionEvent::ToolResult {
                            id: tc.id.clone(),
                            success,
                            output,
                            topology_signature: None,
                        });

                        // The verdict lands in the check panel (the app
                        // projects `CheckResult` into its check items).
                        self.inner.session.append(SessionEvent::CheckResult {
                            check_id,
                            passed: outcome.accepted,
                            details: check_details,
                        });
                    }
                }

                // step/end
                self.inner
                    .session
                    .append(SessionEvent::StepEnd { turn, step });

                // Step continuation: tool debt or steering.
                let mut continue_step = has_tool_calls || self.inner.inbox.has_step();

                // Before the turn ends, grade the plan. A failure with retry
                // budget left becomes a steering message, so the turn corrects
                // the result instead of ending on a wrong one.
                if !continue_step {
                    let allow_retry = verify_retries < MAX_VERIFY_RETRIES;
                    if let PlanVerdict::Failed {
                        diagnostics,
                        retry_allowed: true,
                        ..
                    } = self.grade_and_finalize_plan(allow_retry)
                    {
                        verify_retries += 1;
                        self.inner
                            .inbox
                            .push_step(verification_feedback(&diagnostics));
                        continue_step = true;
                    }
                }

                if continue_step {
                    step += 1;
                    if step >= MAX_STEPS {
                        // The model is looping; cut the turn off rather than
                        // let it run without bound.
                        turn_reason = TurnEndReason::MaxSteps;
                        break;
                    }
                    *self.inner.state.lock().unwrap() = AgentState::Running { turn, step };
                } else {
                    break;
                }
            }

            // A turn that ended early (max steps, provider error) still has to
            // finalize a plan left in Executing. Paths that graded already
            // return NoPlan here, so this never double-counts.
            self.grade_and_finalize_plan(false);

            // turn/end
            self.inner.session.append(SessionEvent::TurnEnd {
                turn,
                reason: turn_reason,
            });
            *self.inner.state.lock().unwrap() = AgentState::Idle;
        }
    }
}

/// It names each failing measurement rather than the aggregate count, so the
/// model can act on what actually went wrong.
fn verification_feedback(diagnostics: &[String]) -> String {
    let mut message = String::from(
        "The plan's acceptance checks did not pass. Correct the result; do not repeat the same call unchanged.\n\nFailing checks:",
    );
    if diagnostics.is_empty() {
        message.push_str("\n- (no check detail was recorded)");
    } else {
        for diagnostic in diagnostics {
            message.push_str("\n- ");
            message.push_str(diagnostic);
        }
    }
    message
}
