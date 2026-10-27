//! Harness assembly: the framework's builder (design: the shape mirrors
//! workbench-rs's `WorkbenchAppBuilder`).
//!
//! Every `with_*` input is a product type — the framework's types never name
//! the product. `build()` wires the kernel crates into a live agent (the
//! same wiring the ocad `agent_host::assemble` used to do by hand):
//!
//! 1. the session, hydrated from the project's event log when one exists;
//! 2. persistence + the audit side log under the project paths;
//! 3. the LLM runtime (product adapters, or the profile's `[provider]`, or
//!    the headless [`ScriptedAdapter`] default);
//! 4. the tool registry + the approval policy (profile `[defaults]`, else
//!    auto-approve);
//! 5. the skill runner, each skill enabled **inside `catch_unwind`** — one
//!    plugin's panic must not sink the assembly (failures are collected in
//!    the assembly notes instead);
//! 6. the `prompt/assemble` bridge to the cordis waterfall, so enabled
//!    skills actually contribute their persona sections.
//!
//! The session's clean-marker discipline (the crash-recovery probe's
//! substrate): a normal exit runs [`Drop`], which writes `clean.marker` into
//! the conversation directory; a crash never does. The next `build()` finds
//! the log without the marker and reports [`Harness::crash_recovered`].

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use nanobot_agent::{Agent, PlanVerifier, ReplayOutcome, ToolExecutor};
use nanobot_cordis::Context;
use nanobot_llm::{LlmAdapter, LlmRuntime, ScriptedAdapter};
#[cfg(feature = "nanobot-llm-providers")]
use nanobot_llm::OpenAiCompatibleAdapter;
#[cfg(not(target_family = "wasm"))]
use nanobot_session::persistence;
use nanobot_session::{DanglingCall, Session, SessionEvent, dangling_tool_calls};
use nanobot_skills::{PromptAssemble, SkillManifest, SkillRunner};
use nanobot_telemetry::TelemetryContext;
use nanobot_tools::{
    ApprovalPolicy, AutoApprovalPolicy, DenyAllPolicy, ToolContract, ToolDefinition, ToolPipeline,
    ToolRegistry, UiApprovalPolicy,
};

use crate::account::{Account, AccountProvider, AnonymousProvider, EntitlementDecision, SharedAccountProvider, TURN_CAPABILITY};
use crate::container::ProjectPaths;
use crate::error::BootError;
use crate::harness_toml::{ApprovalMode, HarnessProfile};
use crate::settings::{Settings, SETTINGS_FILE};

/// Whether re-running an interrupted call is safe.
///
/// Read-only and ungated. A mutating call could double-apply whatever the
/// crashed run already did, and a gated call cannot re-obtain the approval the
/// crash lost. A call whose tool is gone cannot be replayed at all.
fn replay_is_safe(registry: &ToolRegistry, call: &DanglingCall) -> Result<(), String> {
    let definition = registry
        .get(&call.name)
        .ok_or_else(|| format!("tool `{}` is no longer registered", call.name))?;
    if definition.requires_approval {
        return Err(format!(
            "tool `{}` requires approval, which a replay cannot re-obtain",
            call.name
        ));
    }
    if !matches!(definition.contract, ToolContract::ReadOnly) {
        return Err(format!(
            "tool `{}` mutates state; re-running it could double-apply",
            call.name
        ));
    }
    Ok(())
}

/// Answer a dangling call as unknown, so the log is a valid turn again.
///
/// The model can see that the call never returned and re-issue it.
fn record_unknown_outcome(session: &Session, call: &DanglingCall, reason: &str) {
    session.append(SessionEvent::ToolResult {
        id: call.id.clone(),
        success: false,
        output: serde_json::json!({
            "ok": false,
            "error": {
                "code": "interrupted",
                "detail": "the previous run was interrupted before this call returned, so its \
                           result is unknown; re-issue it",
                "reason": reason,
            }
        })
        .to_string(),
    });
}

/// The marker [`Drop`] writes into the conversation directory. Its absence
/// at the next `build()` — with a log present — is what
/// [`Harness::crash_recovered`] reports.
pub const CLEAN_MARKER: &str = "clean.marker";

/// The assembled harness: a live [`Agent`] with its skill runner and the
/// cordis context, plus the assembly's identity and notes.
pub struct Harness {
    pub agent: Agent,
    pub skills: SkillRunner,
    /// The cordis root context the skill runner listens on.
    pub context: Context,
    profile: HarnessProfile,
    profile_source: Option<PathBuf>,
    paths: Option<ProjectPaths>,
    settings: Arc<Mutex<Settings>>,
    settings_file: PathBuf,
    account_provider: SharedAccountProvider,
    session_id: String,
    tool_count: usize,
    has_tool_executor: bool,
    notes: Vec<String>,
    recovered: bool,
    /// Calls a crash left unanswered that are safe to replay, waiting for the
    /// caller to drive them (`resume()`).
    pending_resume: Mutex<Vec<PendingResume>>,
    /// How recovery resolved every dangling call it found.
    repaired: Mutex<Vec<RepairedCall>>,
}

/// A crash-interrupted tool call that recovery decided to replay.
#[derive(Debug, Clone, PartialEq)]
pub struct PendingResume {
    /// The call id the missing `ToolResult` would have carried.
    pub id: String,
    /// The tool the call named.
    pub name: String,
    /// The arguments the model sent.
    pub arguments: serde_json::Value,
    /// The log position of the original call.
    pub seq: u32,
}

/// How recovery resolved one dangling call.
#[derive(Debug, Clone, PartialEq)]
pub enum RepairedCall {
    /// Replayed and answered.
    Replayed {
        /// The call id.
        id: String,
        /// Whether the tool reported success.
        success: bool,
    },
    /// Could not be safely replayed, so its outcome was recorded as unknown.
    Unknown {
        /// The call id.
        id: String,
        /// Why it was not replayed.
        reason: String,
    },
}

/// How many interrupted calls one recovery may replay.
///
/// A bound, because replaying is re-execution: a session that somehow reports
/// many dangling calls must not turn recovery into a long silent run.
pub const RESUME_BUDGET: usize = 3;

impl Harness {
    /// Starts a builder with the built-in defaults.
    pub fn builder() -> HarnessBuilder {
        HarnessBuilder::new()
    }

    /// The session id this harness was assembled for.
    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    /// The identity profile (loaded or built-in defaults).
    pub fn profile(&self) -> &HarnessProfile {
        &self.profile
    }

    /// The file the profile was loaded from, when it came from a
    /// `harness.toml` on disk.
    pub fn profile_source(&self) -> Option<&Path> {
        self.profile_source.as_deref()
    }

    /// The project paths, when the product configured them.
    pub fn paths(&self) -> Option<&ProjectPaths> {
        self.paths.as_ref()
    }

    /// The session's event log path (`None` without project paths).
    pub fn events_path(&self) -> Option<PathBuf> {
        self.paths
            .as_ref()
            .map(|p| p.conversation(&self.session_id).join("events.jsonl"))
    }

    /// The session's audit side log path (`None` without project paths).
    pub fn audit_path(&self) -> Option<PathBuf> {
        self.paths
            .as_ref()
            .map(|p| p.audit().join(format!("{}.jsonl", self.session_id)))
    }

    /// The user settings store, shared with the shell's settings panel.
    /// Changes write through; [`Drop`] persists anything still pending. The
    /// store is user-scoped (`~/.config/<harness id>/`), not project-scoped —
    /// the theme survives a project switch.
    pub fn settings(&self) -> Arc<Mutex<Settings>> {
        self.settings.clone()
    }

    /// The file the user settings persist to.
    pub fn settings_path(&self) -> &Path {
        &self.settings_file
    }

    /// The identity snapshot from the account provider (`None` = not signed
    /// in; the default [`AnonymousProvider`] never signs in).
    pub fn account(&self) -> Option<Account> {
        self.account_provider.account()
    }

    /// The account provider itself (products with their own gating copy the
    /// seam: ask, then audit the denial the same way).
    pub fn account_provider(&self) -> &SharedAccountProvider {
        &self.account_provider
    }

    /// Whether `capability` may run. A denial is **auditable, not silent**:
    /// it lands on the session log as `CheckResult { check_id:
    /// "account:<capability>" }` with the reason in `details` and structure
    /// in `payload`, so an operator replaying the log sees every refusal
    /// next to the checks that passed.
    pub fn check_entitlement(&self, capability: &str) -> EntitlementDecision {
        let decision = self.account_provider.check(capability);
        if let EntitlementDecision::Denied { reason } = &decision {
            for event in crate::account::denial_events(capability, reason) {
                self.agent.session().append(event);
            }
        }
        decision
    }

    /// How many tool definitions the registry carries.
    pub fn tool_count(&self) -> usize {
        self.tool_count
    }

    /// Whether a tool execution backend was wired.
    pub fn has_tool_executor(&self) -> bool {
        self.has_tool_executor
    }

    /// Notes the assembly collected: skipped skills, provider fallbacks.
    /// A note is never fatal — assembly succeeds with what it could wire.
    pub fn assembly_notes(&self) -> &[String] {
        &self.notes
    }

    /// Whether the previous run of this session ended uncleanly (a log
    /// exists but the clean marker does not). The log was already hydrated
    /// into the session by `build()`.
    pub fn crash_recovered(&self) -> bool {
        self.recovered
    }

    /// Whether the previous run left tool calls that are safe to replay.
    ///
    /// A dangling call makes the log an invalid turn for every provider, so
    /// recovery resolves *all* of them: the safe ones are queued here, the rest
    /// are answered as unknown immediately. The caller decides when to drain
    /// the queue, because driving a run has to happen wherever the output is
    /// rendered.
    pub fn has_pending_resume(&self) -> bool {
        !self.pending_resume.lock().unwrap().is_empty()
    }

    /// The calls waiting to be replayed.
    pub fn pending_resume(&self) -> Vec<PendingResume> {
        self.pending_resume.lock().unwrap().clone()
    }

    /// How recovery resolved each dangling call it found.
    pub fn repaired_calls(&self) -> Vec<RepairedCall> {
        self.repaired.lock().unwrap().clone()
    }

    /// Replay the queued calls, answering each in the log.
    ///
    /// Returns how many ran. The queue drains: a call that could not be
    /// replayed is logged as refused rather than left pending.
    pub fn resume(&self) -> usize {
        let pending: Vec<PendingResume> = {
            let mut queue = self.pending_resume.lock().unwrap();
            std::mem::take(&mut *queue)
        };

        let mut replayed = 0;
        for call in pending {
            let outcome = self.agent.replay_call(&call.id, &call.name, &call.arguments);
            let repaired = match outcome {
                ReplayOutcome::Replayed { success, .. } => {
                    replayed += 1;
                    RepairedCall::Replayed {
                        id: call.id,
                        success,
                    }
                }
                ReplayOutcome::Unknown => RepairedCall::Unknown {
                    id: call.id,
                    reason: "the call was refused during replay".to_string(),
                },
            };
            self.repaired.lock().unwrap().push(repaired);
        }
        replayed
    }

    /// The conversation directory's clean marker path (`None` without
    /// project paths).
    fn clean_marker_path(&self) -> Option<PathBuf> {
        self.paths
            .as_ref()
            .map(|p| p.conversation(&self.session_id).join(CLEAN_MARKER))
    }
}

impl Drop for Harness {
    fn drop(&mut self) {
        // A normal exit marks the session clean. A crash (process exit
        // without drop) leaves the marker absent — the next build() reads
        // the absence as "recover". Settings need no close-out: they are
        // written through at every change.
        if let Some(marker) = self.clean_marker_path() {
            if let Some(dir) = marker.parent() {
                let _ = std::fs::create_dir_all(dir);
            }
            let _ = std::fs::write(&marker, b"");
        }
    }
}

/// The harness builder. Defaults: session id `"default"`, built-in identity
/// profile, scripted LLM adapter, auto approval, no tools, no skills.
pub struct HarnessBuilder {
    session_id: String,
    paths: Option<ProjectPaths>,
    profile: HarnessProfile,
    profile_source: Option<PathBuf>,
    settings_path: Option<PathBuf>,
    account_provider: Option<SharedAccountProvider>,
    adapters: Vec<(String, Arc<dyn LlmAdapter>)>,
    registry: Option<Arc<ToolRegistry>>,
    tool_definitions: Vec<ToolDefinition>,
    tool_executor: Option<Arc<dyn ToolExecutor>>,
    plan_verifier: Option<Arc<dyn PlanVerifier>>,
    approval_policy: Option<Arc<dyn ApprovalPolicy>>,
    persona: Option<String>,
    model: Option<String>,
    /// Model routes declared by the product: `(model id, provider key, wire model)`.
    model_routes: Vec<(String, String, String)>,
    /// The projected-history threshold for compaction; `0` disables it.
    compaction_threshold: Option<usize>,
    skills: Vec<SkillManifest>,
    telemetry: Option<Arc<dyn TelemetryContext>>,
}

impl HarnessBuilder {
    pub fn new() -> Self {
        Self {
            session_id: "default".to_string(),
            paths: None,
            profile: HarnessProfile::default(),
            profile_source: None,
            settings_path: None,
            account_provider: None,
            adapters: Vec::new(),
            registry: None,
            tool_definitions: Vec::new(),
            tool_executor: None,
            plan_verifier: None,
            approval_policy: None,
            persona: None,
            model: None,
            model_routes: Vec::new(),
            compaction_threshold: None,
            skills: Vec::new(),
            telemetry: None,
        }
    }

    /// The session id the harness is assembled for (each activated session
    /// gets its own harness).
    pub fn with_session_id(mut self, session_id: impl Into<String>) -> Self {
        self.session_id = session_id.into();
        self
    }

    /// The project paths: the session's event log and audit side log persist
    /// under them, and the clean-marker crash discipline engages.
    pub fn with_project_paths(mut self, paths: ProjectPaths) -> Self {
        self.paths = Some(paths);
        self
    }

    /// The identity profile (from `harness.toml`, or the product's built-in
    /// default). Sets the approval policy and the provider unless the
    /// product overrides them explicitly.
    pub fn with_harness_profile(mut self, profile: HarnessProfile) -> Self {
        self.profile = profile;
        self
    }

    /// Where the profile was loaded from (the selfcheck's identity probe
    /// reports it).
    pub fn with_harness_profile_source(mut self, path: PathBuf) -> Self {
        self.profile_source = Some(path);
        self
    }

    /// Where the user settings persist. When unset, the conventional
    /// user-scoped file for the profile's identity is used
    /// (`~/.config/<harness id>/settings.json`). Products pin this in tests
    /// so an assembly never touches the real user config directory.
    pub fn with_settings_path(mut self, path: PathBuf) -> Self {
        self.settings_path = Some(path);
        self
    }

    /// The account backend (the product's authentication seam). When unset,
    /// [`AnonymousProvider`] applies: not signed in, every capability
    /// allowed — the behaviour every existing consumer already has.
    pub fn with_account_provider(mut self, provider: Arc<dyn AccountProvider>) -> Self {
        self.account_provider = Some(provider);
        self
    }

    /// Registers an LLM adapter under `key` (the agent requests the
    /// `"default"` key today).
    pub fn with_llm_adapter(
        mut self,
        key: impl Into<String>,
        adapter: Arc<dyn LlmAdapter>,
    ) -> Self {
        self.adapters.push((key.into(), adapter));
        self
    }

    /// Replaces the tool registry wholesale (the product owns the registry
    /// when it wants to).
    pub fn with_tool_registry(mut self, registry: Arc<ToolRegistry>) -> Self {
        self.registry = Some(registry);
        self
    }

    /// Registers tool definitions into the built-in registry. The actual
    /// execution is dispatched through the tool executor.
    pub fn with_tool_definitions(mut self, definitions: Vec<ToolDefinition>) -> Self {
        self.tool_definitions.extend(definitions);
        self
    }

    /// The tool execution backend (the product's engine seam).
    pub fn with_tool_executor(mut self, executor: Arc<dyn ToolExecutor>) -> Self {
        self.tool_executor = Some(executor);
        self
    }

    /// The plan intent grader (the product's verifier seam).
    pub fn with_plan_verifier(mut self, verifier: Arc<dyn PlanVerifier>) -> Self {
        self.plan_verifier = Some(verifier);
        self
    }

    /// The approval policy. When unset, the profile's `[defaults].approval`
    /// decides (default: auto).
    pub fn with_approval_policy(mut self, policy: Arc<dyn ApprovalPolicy>) -> Self {
        self.approval_policy = Some(policy);
        self
    }

    /// The product's standing instructions (persona section of the system
    /// prompt). When unset the framework's built-in default is used; a product
    /// supplies its own because the domain identity belongs to the product.
    pub fn with_persona(mut self, persona: impl Into<String>) -> Self {
        self.persona = Some(persona.into());
        self
    }

    /// The wire model id the agent asks the provider for, and the id recorded
    /// on `RequestHeader`. When unset the agent asks for the adapter key
    /// `"default"`, which a real provider will not recognise — a product
    /// talking to a real model sets this.
    pub fn with_model(mut self, model: impl Into<String>) -> Self {
        self.model = Some(model.into());
        self
    }

    /// Declare a model route on the assembled harness's runtime.
    ///
    /// `id` is the model id the agent is pointed at with
    /// [`HarnessBuilder::with_model`]; `provider` is the adapter key it routes
    /// to; `model` is the wire model the provider is asked for. The wire model
    /// may differ from the id, which is how a harness offers an alias (a cost
    /// or latency tier) without naming a concrete provider model.
    ///
    /// A harness that declares no routes keeps the pre-catalog behaviour: the
    /// model id is the wire model and it routes to the `default` adapter.
    pub fn with_model_route(
        mut self,
        id: impl Into<String>,
        provider: impl Into<String>,
        model: impl Into<String>,
    ) -> Self {
        self.model_routes
            .push((id.into(), provider.into(), model.into()));
        self
    }

    /// The projected-history threshold for compaction.
    ///
    /// When the model-visible history grows past this, the older turns are
    /// summarized into a `CompactionEntry` so the context stays bounded; the
    /// log keeps every event. `0` disables compaction. Unset, the agent's
    /// `nanobot_agent::DEFAULT_COMPACTION_THRESHOLD` applies.
    pub fn with_compaction_threshold(mut self, max_messages: usize) -> Self {
        self.compaction_threshold = Some(max_messages);
        self
    }

    /// The telemetry context the assembled harness emits its spans through.
    ///
    /// Installed on both the LLM runtime (`provider.request`) and the agent
    /// loop (`agent.turn` / `agent.step` / `tool.execute`). Absent, every span
    /// is dropped on the floor by the no-op default — instrumentation is a
    /// choice, and the kernel never drags a tracing backend into a consumer
    /// that did not ask for one.
    pub fn with_telemetry(mut self, telemetry: Arc<dyn TelemetryContext>) -> Self {
        self.telemetry = Some(telemetry);
        self
    }

    /// Skill manifests to enable at assembly time. Each enablement is
    /// isolated: a manifest that fails (or panics) is skipped and noted, not
    /// fatal.
    pub fn with_skills(mut self, skills: impl IntoIterator<Item = SkillManifest>) -> Self {
        self.skills.extend(skills);
        self
    }

    /// Assembles the harness.
    pub fn build(self) -> Result<Harness, BootError> {
        let Self {
            session_id,
            paths,
            profile,
            profile_source,
            settings_path,
            account_provider,
            adapters,
            registry,
            tool_definitions,
            tool_executor,
            plan_verifier,
            approval_policy,
            persona,
            model,
            model_routes,
            compaction_threshold,
            skills,
            telemetry,
        } = self;

        let mut notes: Vec<String> = Vec::new();

        let account_provider =
            account_provider.unwrap_or_else(|| Arc::new(AnonymousProvider));

        // User settings: user-scoped (keyed by the harness identity, not the
        // project) and never fatal to load — a corrupt file resets and lands
        // in the notes.
        let settings_file = settings_path.unwrap_or_else(|| {
            crate::settings::user_config_dir(&profile.id).join(SETTINGS_FILE)
        });
        let (settings, settings_error) = Settings::load(&settings_file);
        if let Some(error) = settings_error {
            notes.push(error);
        }
        let settings = Arc::new(Mutex::new(settings));

        let session = Session::new(session_id.clone());

        // Recovery detection runs BEFORE persistence attaches: the
        // persistence subscription re-saves the (initially empty) log, which
        // would erase the very file recovery needs to read. Native only:
        // on the web there is no project file to recover — the host owns
        // persistence (boot's project container is a native-shape concept).
        #[cfg(not(target_family = "wasm"))]
        let mut recovered = false;
        #[cfg(target_family = "wasm")]
        let recovered = false;
        #[cfg(not(target_family = "wasm"))]
        if let Some(paths) = &paths {
            let events = paths.conversation(&session_id).join("events.jsonl");
            let marker = paths.conversation(&session_id).join(CLEAN_MARKER);
            if events.is_file() {
                match persistence::load(&events) {
                    Ok(entries) if !entries.is_empty() => {
                        session.hydrate(entries);
                        if !marker.exists() {
                            recovered = true;
                            notes.push(format!(
                                "session log recovered: `{session_id}` has a log but no clean marker"
                            ));
                        }
                    }
                    Ok(_) => {}
                    Err(e) => notes.push(format!(
                        "session log at {} could not be read ({e}); starting empty",
                        events.display()
                    )),
                }
            }
        }

        // Persistence + audit: the subscriptions live in the session, so both
        // logs keep being written for as long as the session does.
        if let Some(paths) = &paths {
            session.persist_to(paths.conversation(&session_id).join("events.jsonl"));
            // The audit trail is the record of what was *attempted* — it keeps
            // the calls that were refused or rolled back and so left no trace
            // in the document.
            session.audit_to(paths.audit().join(format!("{session_id}.jsonl")));
        }

        let mut llm = LlmRuntime::new();
        if adapters.is_empty() {
            if let Some(provider) = &profile.provider {
                #[cfg(feature = "nanobot-llm-providers")]
                if provider.kind == "openai-compatible" {
                    let adapter = OpenAiCompatibleAdapter::new(
                        provider.endpoint.clone(),
                        provider.api_key.clone().unwrap_or_default(),
                    );
                    llm.register("default", Arc::new(adapter));
                    notes.push("provider from harness.toml [provider] (openai-compatible)".to_string());
                } else {
                    notes.push(format!(
                        "harness.toml provider kind `{}` is not built in; using the scripted default",
                        provider.kind
                    ));
                }
                #[cfg(not(feature = "nanobot-llm-providers"))]
                if provider.kind == "openai-compatible" {
                    notes.push(
                        "harness.toml asks for an openai-compatible provider, but this build has \
                         no HTTP provider (no `providers` feature); using the scripted default"
                            .to_string(),
                    );
                }
            }
            if llm.providers().next().is_none() {
                // The headless default: a fixed text response, suitable for
                // CI, selfcheck, and smoke runs.
                llm.register("default", Arc::new(ScriptedAdapter::text("agent response")));
            }
        } else {
            for (key, adapter) in adapters {
                llm.register(key, adapter);
            }
        }

        // The runtime owns the `provider.request` span, so it needs the context
        // before the agent takes ownership of it.
        if let Some(telemetry) = &telemetry {
            llm.set_telemetry(telemetry.clone());
        }

        // Declare the model routes: each maps a model id the agent can be
        // pointed at to the adapter key and the wire model it resolves to.
        for (id, provider, model) in &model_routes {
            llm.register_model(id, provider, model);
        }
        if !model_routes.is_empty() {
            notes.push(format!("{} model route(s) declared", model_routes.len()));
        }

        let mut tool_count = 0usize;
        let registry = match registry {
            Some(registry) => {
                tool_count = registry.len();
                registry
            }
            None => {
                let built = Arc::new(ToolRegistry::new());
                for definition in tool_definitions {
                    tool_count += 1;
                    let _ = built.register(definition);
                }
                built
            }
        };

        let approval_policy = approval_policy.unwrap_or_else(|| match profile.approval {
            ApprovalMode::Auto => Arc::new(AutoApprovalPolicy) as Arc<dyn ApprovalPolicy>,
            ApprovalMode::DenyAll => Arc::new(DenyAllPolicy) as Arc<dyn ApprovalPolicy>,
            ApprovalMode::Ui => Arc::new(UiApprovalPolicy::new(|_tool, _args| false)) as Arc<dyn ApprovalPolicy>,
        });

        let pipeline = ToolPipeline::new(registry.clone(), approval_policy);
        let agent = Agent::new(session, llm, pipeline);
        let has_tool_executor = tool_executor.is_some();

        if let Some(executor) = tool_executor {
            agent.set_tool_executor(executor);
        }
        if let Some(verifier) = plan_verifier {
            agent.set_plan_verifier(verifier);
        }
        if let Some(persona) = persona {
            agent.set_persona(persona);
        }
        if let Some(model) = model {
            agent.set_model(model);
        }
        if let Some(telemetry) = &telemetry {
            agent.set_telemetry(telemetry.clone());
        }
        if let Some(threshold) = compaction_threshold {
            agent.set_compaction_threshold(threshold);
        }

        // The skill runner with a root context. Each enablement is isolated:
        // one plugin's failure (or panic) must not sink the assembly.
        let context = Context::root();
        let skills_runner = SkillRunner::new(context.clone());
        for manifest in skills {
            let name = manifest.package.name.clone();
            let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                skills_runner.enable(&manifest)
            }));
            match outcome {
                Ok(Ok(_)) => {}
                Ok(Err(error)) => notes.push(format!("skill `{name}` rejected: {error}")),
                Err(_) => notes.push(format!("skill `{name}` panicked during enable; skipped")),
            }
        }

        // Bridge the agent's `prompt/assemble` hook to the cordis waterfall,
        // so the enabled skills actually contribute their persona sections.
        // Without this bridge the skills are enabled but inert.
        let bridge_context = context.clone();
        agent.set_prompt_assembler(move |system_prompt| {
            let base = system_prompt.clone();
            bridge_context
                .events()
                .waterfall::<PromptAssemble, PromptAssemble>(
                    "prompt/assemble",
                    PromptAssemble { system_prompt },
                )
                .map(|assembled| assembled.system_prompt)
                .unwrap_or(base)
        });

        // Resolve the calls a crash left unanswered. This runs after the
        // registry exists (the safety decision reads it) and after persistence
        // attaches (the repair appends are written).
        //
        // Every dangling call must be answered: a call without its result makes
        // the log an invalid turn for every provider, so the next request
        // would be rejected. The safe ones wait for the caller to replay them;
        // the rest are answered as unknown right now.
        let mut pending_resume = Vec::new();
        let mut repaired = Vec::new();
        if recovered {
            for call in dangling_tool_calls(&agent.session().events()) {
                let safe = replay_is_safe(&registry, &call);
                match safe {
                    Ok(()) if pending_resume.len() < RESUME_BUDGET => {
                        pending_resume.push(PendingResume {
                            id: call.id.clone(),
                            name: call.name.clone(),
                            arguments: call.arguments.clone(),
                            seq: call.seq,
                        });
                        notes.push(format!(
                            "interrupted call `{}` ({}) is safe to replay; call `resume()`",
                            call.name, call.id
                        ));
                    }
                    Ok(()) => {
                        let reason = format!(
                            "the resume budget of {RESUME_BUDGET} is spent, so it was not replayed"
                        );
                        record_unknown_outcome(agent.session(), &call, &reason);
                        repaired.push(RepairedCall::Unknown {
                            id: call.id.clone(),
                            reason,
                        });
                    }
                    Err(reason) => {
                        record_unknown_outcome(agent.session(), &call, &reason);
                        repaired.push(RepairedCall::Unknown {
                            id: call.id.clone(),
                            reason,
                        });
                    }
                }
            }
            if !repaired.is_empty() {
                notes.push(format!(
                    "{} interrupted call(s) recorded as unknown; re-issue them",
                    repaired.len()
                ));
            }
        }

        Ok(Harness {
            agent,
            skills: skills_runner,
            context,
            profile,
            profile_source,
            paths,
            settings,
            settings_file,
            account_provider,
            session_id,
            tool_count,
            has_tool_executor,
            notes,
            recovered,
            pending_resume: Mutex::new(pending_resume),
            repaired: Mutex::new(repaired),
        })
    }
}

impl Default for HarnessBuilder {
    fn default() -> Self {
        Self::new()
    }
}

/// Convenience used by the selfcheck: drives one user turn through the
/// harness's agent, headlessly (the loop runs on the local executor).
///
/// The turn is gated on [`crate::account::TURN_CAPABILITY`]: a denied
/// account records the refusal on the session log (a failed
/// `account:agent.turn` check) and the turn does not run.
pub fn run_one_turn(harness: &Harness, message: &str) {
    if !harness.check_entitlement(TURN_CAPABILITY).is_allowed() {
        return;
    }
    harness.agent.followup(message.to_string());
    futures_executor::block_on(harness.agent.run());
}

/// Whether `event` is a user message carrying exactly `content` (the
/// selfcheck's persistence probe looks for the turn it just drove).
pub fn is_user_message(event: &SessionEvent, content: &str) -> bool {
    matches!(&event, SessionEvent::UserMessage { content: c, .. } if c == content)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::container::ProjectPaths;

    fn scratch(label: &str) -> ProjectPaths {
        let root =
            std::env::temp_dir().join(format!("nb-harness-{label}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        ProjectPaths::new(&root)
    }

    #[test]
    fn headless_default_assembly_runs_a_turn() {
        let harness = Harness::builder()
            .with_session_id("sess-default")
            .build()
            .expect("assembly");
        assert_eq!(harness.profile().id, "unnamed.harness");
        assert!(harness.tool_count() == 0);
        assert!(!harness.has_tool_executor());

        run_one_turn(&harness, "hello");
        assert!(
            !harness.agent.session().is_empty(),
            "the scripted default must produce a turn"
        );
    }

    #[test]
    fn a_declared_model_route_sends_the_wire_model_to_its_provider() {
        // The builder is the production path: the product declares the route
        // and the model id, and the agent resolves the id to the provider and
        // the wire model the provider is asked for.
        let harness = Harness::builder()
            .with_session_id("sess-route")
            .with_llm_adapter("openai", Arc::new(ScriptedAdapter::text("ok")))
            .with_model_route("fast", "openai", "gpt-6-mini")
            .with_model("fast")
            .build()
            .expect("assembly");

        run_one_turn(&harness, "go");

        let recorded = harness
            .agent
            .session()
            .events()
            .into_iter()
            .find_map(|entry| match entry.event {
                SessionEvent::RequestHeader { model, .. } => Some(model),
                _ => None,
            });
        assert_eq!(
            recorded,
            Some("gpt-6-mini".to_string()),
            "`with_model_route` routes the alias to its provider and records its wire model"
        );
    }

    #[test]
    fn persistence_and_recovery_round_trip_through_the_clean_marker() {
        let paths = scratch("marker");
        let session_id = "sess-marker";

        // First build: fresh log, no recovery. A turn lands in the file.
        let events_after_first;
        {
            let harness = Harness::builder()
                .with_session_id(session_id)
                .with_project_paths(paths.clone())
                .build()
                .expect("first build");
            assert!(!harness.crash_recovered());
            assert!(harness.events_path().expect("events path").is_file());
            run_one_turn(&harness, "recorded turn");
            events_after_first = harness.agent.session().len();
        }
        // Normal drop wrote the marker.
        let marker = paths.conversation(session_id).join(CLEAN_MARKER);
        assert!(marker.exists(), "a dropped harness marks its session clean");

        // Second build: marker present, log hydrated (recovery suppressed).
        {
            let harness = Harness::builder()
                .with_session_id(session_id)
                .with_project_paths(paths.clone())
                .build()
                .expect("second build");
            assert!(!harness.crash_recovered());
            assert_eq!(
                harness.agent.session().len(),
                events_after_first,
                "the log was hydrated"
            );
        }

        // Crash simulation: remove the marker, rebuild — recovery fires.
        std::fs::remove_file(&marker).unwrap();
        let harness = Harness::builder()
            .with_session_id(session_id)
            .with_project_paths(paths.clone())
            .build()
            .expect("third build");
        assert!(harness.crash_recovered());
        assert!(
            !harness.assembly_notes().is_empty(),
            "recovery is reported in the assembly notes"
        );

        let _ = std::fs::remove_dir_all(paths.root());
    }

    #[test]
    fn hydrated_log_resaves_byte_identical() {
        let paths = scratch("bytes");
        let session_id = "sess-bytes";
        {
            let harness = Harness::builder()
                .with_session_id(session_id)
                .with_project_paths(paths.clone())
                .build()
                .expect("build");
            run_one_turn(&harness, "byte check");
        }
        let events = paths.conversation(session_id).join("events.jsonl");
        let before = std::fs::read(&events).unwrap();
        std::fs::remove_file(paths.conversation(session_id).join(CLEAN_MARKER)).unwrap();

        let _harness = Harness::builder()
            .with_session_id(session_id)
            .with_project_paths(paths.clone())
            .build()
            .expect("rebuild");
        let after = std::fs::read(&events).unwrap();
        assert_eq!(
            before, after,
            "hydrate + re-save must be byte-identical (seq and timestamp preserved)"
        );

        let _ = std::fs::remove_dir_all(paths.root());
    }

    #[test]
    fn settings_ride_on_the_harness_and_persist_on_drop() {
        let dir = std::env::temp_dir().join(format!("nb-harness-settings-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let file = dir.join("settings.json");

        {
            let harness = Harness::builder()
                .with_session_id("sess-settings")
                .with_settings_path(file.clone())
                .build()
                .expect("build");
            assert_eq!(harness.settings_path(), file.as_path());
            harness
                .settings()
                .lock()
                .unwrap()
                .set("ui.theme", serde_json::json!("light"))
                .expect("set");
        }
        // The harness dropped: the changed setting is on disk.
        assert!(file.is_file(), "drop persists a changed setting");
        let (settings, error) = Settings::load(&file);
        assert!(error.is_none());
        assert_eq!(settings.get_str("ui.theme", "dark"), "light");

        // A rebuild on the same file sees the user's value.
        let harness = Harness::builder()
            .with_session_id("sess-settings")
            .with_settings_path(file.clone())
            .build()
            .expect("rebuild");
        assert_eq!(
            harness.settings().lock().unwrap().get_str("ui.theme", "dark"),
            "light",
            "the user's setting survives a rebuild"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_denied_turn_is_refused_auditably() {
        use crate::account::{Account, StaticProvider, TURN_CAPABILITY};

        let provider = StaticProvider::new(Account::new("lic-9", "Expired Site").with_expiry_ms(1))
            .with_now_ms(2);

        let harness = Harness::builder()
            .with_session_id("sess-denied")
            .with_account_provider(Arc::new(provider))
            .build()
            .expect("assembly");
        assert_eq!(
            harness.account().map(|account| account.id),
            Some("lic-9".to_string())
        );

        let before = harness.agent.session().len();
        run_one_turn(&harness, "should not run");
        assert_eq!(
            harness.agent.session().len(),
            before + 2,
            "a denied turn appends exactly its audit pair (run + failed result)"
        );

        // The refusal is on the log in the check vocabulary.
        let events = harness.agent.session().events();
        let last = events.last().expect("audit events").event.clone();
        match last {
            SessionEvent::CheckResult { check_id, passed, details, .. } => {
                assert_eq!(check_id, format!("account:{TURN_CAPABILITY}"));
                assert!(!passed);
                assert!(details.contains("expired"), "the reason is the prose: {details}");
            }
            other => panic!("expected a CheckResult denial, got {other:?}"),
        }
    }

    #[test]
    fn an_allowed_account_leaves_the_turn_untouched() {
        use crate::account::{Account, StaticProvider};

        let provider = StaticProvider::new(Account::new("u1", "Alice"));
        let harness = Harness::builder()
            .with_session_id("sess-allowed")
            .with_account_provider(Arc::new(provider))
            .build()
            .expect("assembly");

        run_one_turn(&harness, "hello");
        assert!(
            !harness.agent.session().is_empty(),
            "an allowed account runs the turn exactly as before"
        );
        // And no account checks were recorded — allowals are not events.
        assert!(
            !harness
                .agent
                .session()
                .events()
                .iter()
                .any(|entry| matches!(entry.event, SessionEvent::CheckResult { ref check_id, .. } if check_id.starts_with("account:"))),
            "allowals leave no audit noise"
        );
    }

    #[test]
    fn tool_definitions_and_executor_are_reported() {
        use nanobot_tools::ToolRegistry;

        let registry = Arc::new(ToolRegistry::new());
        let definition = ToolDefinition {
            name: "probe_tool".to_string(),
            description: "probe".to_string(),
            parameters_schema: serde_json::json!({"type": "object", "properties": {}}),
            requires_approval: false,
            contract: nanobot_tools::ToolContract::ReadOnly,
            validate_args: false,
        };
        let _ = registry.register(definition);

        let harness = Harness::builder()
            .with_session_id("sess-tools")
            .with_tool_registry(registry)
            .with_tool_executor(Arc::new(NullExecutor))
            .build()
            .expect("build");
        assert_eq!(harness.tool_count(), 1);
        assert!(harness.has_tool_executor());
    }

    struct NullExecutor;

    impl ToolExecutor for NullExecutor {
        fn execute(&self, _name: &str, _arguments: &serde_json::Value) -> String {
            "{}".to_string()
        }
    }

    /// Counts invocations, for the crash-recovery replay assertions.
    #[derive(Default)]
    struct CountingExecutor {
        calls: std::sync::atomic::AtomicUsize,
    }

    impl ToolExecutor for CountingExecutor {
        fn execute(&self, _name: &str, _arguments: &serde_json::Value) -> String {
            self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            "{}".to_string()
        }
    }

    #[test]
    fn a_rejected_skill_is_skipped_not_fatal() {
        use nanobot_skills::{SkillDefinition, SkillManifest, SkillPackage};

        let skill = || SkillManifest {
            package: SkillPackage {
                name: "duplicate.skill".to_string(),
                version: "0.1.0".to_string(),
                api_version: 1,
            },
            permissions: Default::default(),
            skill: SkillDefinition {
                intent: "probe".to_string(),
                scene: String::new(),
                inputs: Default::default(),
                steps: Vec::new(),
                on_failure: Default::default(),
            },
            validation: None,
        };

        // Two manifests with the same name: the second enablement is
        // rejected. The assembly must survive it and report it.
        let harness = Harness::builder()
            .with_session_id("sess-skill-dup")
            .with_skills(vec![skill(), skill()])
            .build()
            .expect("assembly must survive a rejected skill");
        assert_eq!(harness.skills.len(), 1, "only the first copy is enabled");
        assert!(
            !harness.assembly_notes().is_empty(),
            "the rejected skill is reported in the notes"
        );
    }
    // -- crash recovery: the calls a crash left unanswered --

    /// Write a log by hand: a crash mid-call is not something the happy path
    /// produces, so the fixture writes the events a crashed run would have left.
    fn write_log(dir: &Path, events: Vec<SessionEvent>) {
        let entries: Vec<nanobot_session::LogEntry> = events
            .into_iter()
            .enumerate()
            .map(|(index, event)| nanobot_session::LogEntry {
                seq: index as u32,
                timestamp_ms: 0,
                event,
            })
            .collect();
        std::fs::create_dir_all(dir).expect("the conversation directory");
        nanobot_session::save(&dir.join("events.jsonl"), &entries).expect("write the log");
    }

    fn tool(name: &str, contract: ToolContract, requires_approval: bool) -> ToolDefinition {
        ToolDefinition {
            name: name.to_string(),
            description: "a test tool".to_string(),
            parameters_schema: serde_json::json!({"type": "object"}),
            requires_approval,
            contract,
            validate_args: false,
        }
    }

    fn tool_results(harness: &Harness) -> Vec<String> {
        harness
            .agent
            .session()
            .events()
            .into_iter()
            .filter_map(|entry| match entry.event {
                SessionEvent::ToolResult { output, .. } => Some(output),
                _ => None,
            })
            .collect()
    }

    fn dangling(harness: &Harness) -> usize {
        nanobot_session::dangling_tool_calls(&harness.agent.session().events()).len()
    }

    fn crashed_call(id: &str, name: &str) -> SessionEvent {
        SessionEvent::ToolCall {
            id: id.to_string(),
            name: name.to_string(),
            arguments: serde_json::json!({}),
            seq: 3,
        }
    }

    #[test]
    fn a_mutating_call_is_answered_as_unknown_on_recovery() {
        let paths = scratch("recover-mutating");
        let session_id = "sess-recover-mutating";
        write_log(
            &paths.conversation(session_id),
            vec![
                SessionEvent::TurnStart { turn: 0 },
                SessionEvent::StepStart { turn: 0, step: 0 },
                SessionEvent::UserMessage {
                    content: "build it".to_string(),
                },
                crashed_call("c1", "build_sfq"),
            ],
        );

        let harness = Harness::builder()
            .with_session_id(session_id)
            .with_project_paths(paths)
            .with_tool_definitions(vec![tool("build_sfq", ToolContract::Mutating, true)])
            .build()
            .expect("assembly");

        assert!(harness.crash_recovered());
        assert!(
            !harness.has_pending_resume(),
            "a mutating, gated call must not be queued for replay"
        );
        assert_eq!(harness.repaired_calls().len(), 1);
        assert!(
            dangling(&harness) == 0,
            "the log is a valid turn again: the call has an answer"
        );
        assert!(
            tool_results(&harness)[0].contains("interrupted"),
            "the answer says the result is unknown"
        );
    }

    #[test]
    fn a_read_only_call_is_queued_and_replayed_on_resume() {
        let paths = scratch("recover-read-only");
        let session_id = "sess-recover-read-only";
        write_log(
            &paths.conversation(session_id),
            vec![
                SessionEvent::TurnStart { turn: 0 },
                SessionEvent::StepStart { turn: 0, step: 0 },
                crashed_call("c1", "get_device_catalog"),
            ],
        );

        let executor = Arc::new(CountingExecutor::default());
        let harness = Harness::builder()
            .with_session_id(session_id)
            .with_project_paths(paths)
            .with_tool_definitions(vec![tool(
                "get_device_catalog",
                ToolContract::ReadOnly,
                false,
            )])
            .with_tool_executor(executor.clone())
            .build()
            .expect("assembly");

        assert!(
            harness.has_pending_resume(),
            "a read-only call waits to be replayed"
        );
        assert_eq!(dangling(&harness), 1, "it is not answered until resume() runs");
        assert_eq!(
            harness.pending_resume()[0].name, "get_device_catalog",
            "the queued call names the tool"
        );

        assert_eq!(harness.resume(), 1, "one call replayed");
        assert_eq!(
            executor.calls.load(std::sync::atomic::Ordering::SeqCst),
            1
        );
        assert_eq!(dangling(&harness), 0, "the replay answered it");
        assert!(!harness.has_pending_resume(), "the queue drained");
    }

    #[test]
    fn the_resume_budget_bounds_the_queue() {
        let paths = scratch("recover-budget");
        let session_id = "sess-recover-budget";
        let mut events = vec![
            SessionEvent::TurnStart { turn: 0 },
            SessionEvent::StepStart { turn: 0, step: 0 },
        ];
        for index in 0..(RESUME_BUDGET + 2) {
            events.push(crashed_call(&format!("c{index}"), "get_device_catalog"));
        }
        write_log(&paths.conversation(session_id), events);

        let executor = Arc::new(CountingExecutor::default());
        let harness = Harness::builder()
            .with_session_id(session_id)
            .with_project_paths(paths)
            .with_tool_definitions(vec![tool(
                "get_device_catalog",
                ToolContract::ReadOnly,
                false,
            )])
            .with_tool_executor(executor.clone())
            .build()
            .expect("assembly");

        assert_eq!(
            harness.pending_resume().len(),
            RESUME_BUDGET,
            "the queue is bounded by the resume budget"
        );
        assert_eq!(
            harness.repaired_calls().len(),
            2,
            "the calls past the budget are answered as unknown"
        );
        // The budgeted calls stay open until the caller replays them.
        assert_eq!(dangling(&harness), RESUME_BUDGET);
        assert_eq!(harness.resume(), RESUME_BUDGET);
        assert_eq!(
            executor.calls.load(std::sync::atomic::Ordering::SeqCst),
            RESUME_BUDGET
        );
        assert_eq!(dangling(&harness), 0, "the replay resolved the rest");
    }

    #[test]
    fn the_compaction_threshold_is_wired_from_the_builder() {
        let harness = Harness::builder()
            .with_session_id("sess-compaction-threshold")
            .with_compaction_threshold(7)
            .build()
            .expect("assembly");
        assert_eq!(harness.agent.compaction_threshold(), 7);

        // Unset, the agent's default applies.
        let default = Harness::builder()
            .with_session_id("sess-compaction-default")
            .build()
            .expect("assembly");
        assert_eq!(
            default.agent.compaction_threshold(),
            nanobot_agent::DEFAULT_COMPACTION_THRESHOLD
        );
    }

    #[test]
    fn a_clean_log_repairs_nothing() {
        // A normal exit writes the clean marker, so nothing is recovered.
        let harness = Harness::builder()
            .with_session_id("sess-clean")
            .build()
            .expect("assembly");
        assert!(!harness.crash_recovered());
        assert!(!harness.has_pending_resume());
        assert!(harness.repaired_calls().is_empty());
    }
}
