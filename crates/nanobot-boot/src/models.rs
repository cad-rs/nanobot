//! Model profiles: the models a user configured, and which one a session runs.
//!
//! A [`ModelProfile`] is one way to reach a model: an endpoint, the model the
//! endpoint is asked for, and where its key comes from. The user keeps several
//! in the settings store (`llm.models`), marks one as the default
//! (`llm.default_model`), and each session names the one it runs on by the
//! profile's id — the session keeps that id, not a copy of the profile, so
//! correcting an endpoint corrects every session that uses it.
//!
//! A session whose profile was deleted falls back to the default
//! ([`ModelProfiles::resolve`]); it does not fail.
//!
//! **Keys are not settings.** A key typed into the settings dialog goes to
//! [`CREDENTIALS_FILE`] next to the settings file, never into the settings
//! file itself, which gets shown, exported and copied between machines. A
//! profile can instead name an environment variable (`api_key_env`), and the
//! variable wins when it is set.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::settings::Settings;

/// The settings key of the list of profiles.
pub const MODELS_KEY: &str = "llm.models";
/// The settings key of the default profile's id.
pub const DEFAULT_MODEL_KEY: &str = "llm.default_model";
/// The file next to the settings file that holds the keys typed in the UI.
pub const CREDENTIALS_FILE: &str = "credentials.json";
/// The one wire protocol built in today.
pub const OPENAI_COMPATIBLE: &str = "openai-compatible";

fn openai_compatible() -> String {
    OPENAI_COMPATIBLE.to_string()
}

/// One configured model.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelProfile {
    /// What a session records. Never reused, never shown.
    pub id: String,
    /// What the user calls it.
    pub name: String,
    /// The wire protocol ([`OPENAI_COMPATIBLE`]).
    #[serde(default = "openai_compatible")]
    pub kind: String,
    /// The base URL, e.g. `https://api.deepseek.com/v1`.
    pub endpoint: String,
    /// The model the endpoint is asked for.
    pub model: String,
    /// An environment variable holding the key. Set and non-empty, it wins
    /// over a stored key.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_key_env: Option<String>,
}

impl ModelProfile {
    /// A new profile with a fresh id.
    pub fn new(name: impl Into<String>, endpoint: impl Into<String>, model: impl Into<String>) -> Self {
        Self {
            id: Self::fresh_id(),
            name: name.into(),
            kind: openai_compatible(),
            endpoint: endpoint.into(),
            model: model.into(),
            api_key_env: None,
        }
    }

    /// An id no profile had before: sessions keep ids, so a deleted profile's
    /// id must not come back as another model.
    pub fn fresh_id() -> String {
        use std::sync::atomic::{AtomicU64, Ordering};
        static MADE: AtomicU64 = AtomicU64::new(0);
        // The clock tells runs apart; the count, two in one millisecond.
        let count = MADE.fetch_add(1, Ordering::Relaxed);
        format!("m{:x}-{count:x}", crate::account::now_ms())
    }

    /// Whether there is enough here to make a request.
    pub fn is_complete(&self) -> bool {
        ![&self.name, &self.endpoint, &self.model]
            .iter()
            .any(|field| field.trim().is_empty())
    }

    /// The endpoint's host, as a short way to say where the model lives.
    pub fn host(&self) -> &str {
        let rest = self.endpoint.split("://").nth(1).unwrap_or(&self.endpoint);
        rest.split('/').next().unwrap_or(rest)
    }

    /// The endpoint as the adapter wants it: ending in `/`.
    pub fn base_url(&self) -> String {
        format!("{}/", self.endpoint.trim().trim_end_matches('/'))
    }

    /// The adapter key this profile is registered under in the runtime.
    pub fn adapter_key(&self) -> String {
        format!("profile:{}", self.id)
    }
}

/// The user's profiles and which is the default.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ModelProfiles {
    pub profiles: Vec<ModelProfile>,
    /// The default profile's id. `None`, or naming a profile that is gone,
    /// the first profile is the default.
    pub default: Option<String>,
}

impl ModelProfiles {
    /// Reads the profiles from the settings. Entries that do not parse are
    /// skipped: one bad entry must not hide the rest.
    pub fn load(settings: &Settings) -> Self {
        let profiles = settings
            .get(MODELS_KEY)
            .and_then(Value::as_array)
            .map(|entries| {
                entries
                    .iter()
                    .filter_map(|entry| serde_json::from_value(entry.clone()).ok())
                    .collect()
            })
            .unwrap_or_default();
        let default = settings
            .get(DEFAULT_MODEL_KEY)
            .and_then(Value::as_str)
            .map(str::to_string);
        Self { profiles, default }
    }

    /// Writes the profiles to the settings.
    pub fn store(&self, settings: &mut Settings) -> Result<(), String> {
        let list = serde_json::to_value(&self.profiles).map_err(|e| e.to_string())?;
        settings.set(MODELS_KEY, list)?;
        match &self.default {
            Some(id) => settings.set(DEFAULT_MODEL_KEY, Value::String(id.clone())),
            None => settings.remove(DEFAULT_MODEL_KEY),
        }
    }

    pub fn get(&self, id: &str) -> Option<&ModelProfile> {
        self.profiles.iter().find(|profile| profile.id == id)
    }

    /// The profile a new session starts on.
    pub fn default_profile(&self) -> Option<&ModelProfile> {
        self.default
            .as_deref()
            .and_then(|id| self.get(id))
            .or_else(|| self.profiles.first())
    }

    /// The profile a session that asked for `wanted` runs on: that one while
    /// it exists, the default otherwise.
    pub fn resolve(&self, wanted: Option<&str>) -> Option<&ModelProfile> {
        wanted
            .and_then(|id| self.get(id))
            .or_else(|| self.default_profile())
    }

    /// Adds `profile`, or replaces the one with its id.
    pub fn upsert(&mut self, profile: ModelProfile) {
        match self.profiles.iter_mut().find(|known| known.id == profile.id) {
            Some(known) => *known = profile,
            None => self.profiles.push(profile),
        }
    }

    /// Removes the profile `id`. False when there was none.
    pub fn remove(&mut self, id: &str) -> bool {
        let before = self.profiles.len();
        self.profiles.retain(|profile| profile.id != id);
        if self.default.as_deref() == Some(id) {
            self.default = None;
        }
        self.profiles.len() != before
    }
}

/// Where the keys typed in the UI are kept: next to the settings file. `None`
/// for settings that live in memory only.
fn credentials_path(settings: &Settings) -> Option<PathBuf> {
    settings
        .location()
        .map(|path| PathBuf::from(path).with_file_name(CREDENTIALS_FILE))
}

/// The in-memory stand-in for the credentials file.
fn memory_key(id: &str) -> String {
    format!("llm.key.{id}")
}

fn stored_key(settings: &Settings, id: &str) -> Option<String> {
    let key = match credentials_path(settings) {
        Some(path) => crate::settings::load_settings(&path).0.get_str(id, ""),
        None => settings.get_str(&memory_key(id), ""),
    };
    (!key.is_empty()).then_some(key)
}

/// Whether a key was stored for the profile `id` (the UI says so without
/// ever showing the key).
pub fn has_stored_key(settings: &Settings, id: &str) -> bool {
    stored_key(settings, id).is_some()
}

/// The key requests for `profile` are made with: its environment variable
/// when that is set, the stored key otherwise.
pub fn api_key(settings: &Settings, profile: &ModelProfile) -> Option<String> {
    profile
        .api_key_env
        .as_deref()
        .and_then(|name| std::env::var(name.trim()).ok())
        .filter(|key| !key.trim().is_empty())
        .or_else(|| stored_key(settings, &profile.id))
}

/// Stores the key of the profile `id`, or forgets it (`None`).
pub fn store_api_key(settings: &mut Settings, id: &str, key: Option<&str>) -> Result<(), String> {
    let Some(path) = credentials_path(settings) else {
        return match key {
            Some(key) => settings.set(&memory_key(id), Value::String(key.to_string())),
            None => settings.remove(&memory_key(id)),
        };
    };
    let mut credentials = crate::settings::load_settings(&path).0;
    match key {
        Some(key) => credentials.set(id, Value::String(key.to_string())),
        None if credentials.get(id).is_some() => credentials.remove(id),
        None => Ok(()),
    }
}

/// Makes every profile routable in `llm`: its adapter under
/// [`ModelProfile::adapter_key`], and its id as a model id that routes there.
/// Returns how many were registered (none in a build without the HTTP
/// provider, or of a kind that is not built in).
pub fn register_profiles(
    llm: &mut nanobot_llm::LlmRuntime,
    profiles: &ModelProfiles,
    settings: &Settings,
) -> usize {
    let mut registered = 0;
    for profile in &profiles.profiles {
        if profile.kind != OPENAI_COMPATIBLE || !profile.is_complete() {
            continue;
        }
        #[cfg(feature = "nanobot-llm-providers")]
        {
            let adapter = nanobot_llm::OpenAiCompatibleAdapter::new(
                profile.base_url(),
                api_key(settings, profile).unwrap_or_default(),
            );
            llm.register(profile.adapter_key(), std::sync::Arc::new(adapter));
            llm.register_model(&profile.id, profile.adapter_key(), &profile.model);
            registered += 1;
        }
    }
    #[cfg(not(feature = "nanobot-llm-providers"))]
    let _ = (llm, settings);
    registered
}

#[cfg(test)]
mod tests {
    use super::*;

    fn two() -> ModelProfiles {
        let mut all = ModelProfiles::default();
        all.upsert(ModelProfile {
            id: "a".into(),
            ..ModelProfile::new("Fast", "https://api.example.com/v1", "small")
        });
        all.upsert(ModelProfile {
            id: "b".into(),
            ..ModelProfile::new("Deep", "http://localhost:11434/v1/", "big")
        });
        all
    }

    #[test]
    fn profiles_round_trip_through_the_settings() {
        let mut settings = Settings::in_memory();
        let mut all = two();
        all.default = Some("b".into());
        all.store(&mut settings).unwrap();
        assert_eq!(ModelProfiles::load(&settings), all);

        // Without a default the key is gone, not empty.
        all.default = None;
        all.store(&mut settings).unwrap();
        assert!(settings.get(DEFAULT_MODEL_KEY).is_none());
    }

    #[test]
    fn an_entry_that_does_not_parse_is_skipped() {
        let mut settings = Settings::in_memory();
        settings
            .set(
                MODELS_KEY,
                serde_json::json!([
                    { "nonsense": true },
                    { "id": "a", "name": "A", "endpoint": "http://x/v1", "model": "m" },
                ]),
            )
            .unwrap();
        let all = ModelProfiles::load(&settings);
        assert_eq!(all.profiles.len(), 1);
        assert_eq!(all.profiles[0].kind, OPENAI_COMPATIBLE, "the kind has a default");
    }

    #[test]
    fn a_session_whose_profile_is_gone_runs_on_the_default() {
        let mut all = two();
        assert_eq!(all.resolve(Some("b")).unwrap().id, "b");
        assert_eq!(all.resolve(None).unwrap().id, "a", "the first is the default");
        all.default = Some("b".into());
        assert_eq!(all.resolve(Some("deleted")).unwrap().id, "b");

        // Deleting the default leaves the first as the default.
        assert!(all.remove("b"));
        assert!(!all.remove("b"));
        assert_eq!(all.default, None);
        assert_eq!(all.resolve(Some("b")).unwrap().id, "a");
        assert!(ModelProfiles::default().resolve(Some("a")).is_none());
    }

    #[test]
    fn upsert_replaces_by_id() {
        let mut all = two();
        all.upsert(ModelProfile { id: "a".into(), ..ModelProfile::new("Renamed", "http://x", "m") });
        assert_eq!(all.profiles.len(), 2);
        assert_eq!(all.get("a").unwrap().name, "Renamed");
    }

    #[test]
    fn fresh_ids_differ() {
        assert_ne!(ModelProfile::fresh_id(), ModelProfile::fresh_id());
    }

    #[test]
    fn the_endpoint_is_read_for_its_host_and_its_base_url() {
        let all = two();
        assert_eq!(all.profiles[0].host(), "api.example.com");
        assert_eq!(all.profiles[0].base_url(), "https://api.example.com/v1/");
        assert_eq!(all.profiles[1].host(), "localhost:11434");
        assert_eq!(all.profiles[1].base_url(), "http://localhost:11434/v1/");
        assert!(!ModelProfile::new("", "http://x", "m").is_complete());
    }

    #[test]
    fn a_typed_key_goes_next_to_the_settings_file_not_into_it() {
        let dir = std::env::temp_dir().join(format!("nb-models-key-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let file = dir.join("settings.json");
        let mut settings = crate::settings::load_settings(&file).0;
        let all = two();
        all.store(&mut settings).unwrap();

        assert!(!has_stored_key(&settings, "a"));
        store_api_key(&mut settings, "a", Some("sk-secret")).unwrap();
        assert!(has_stored_key(&settings, "a"));
        assert_eq!(api_key(&settings, &all.profiles[0]).as_deref(), Some("sk-secret"));
        let written = std::fs::read_to_string(&file).unwrap();
        assert!(!written.contains("sk-secret"), "the settings file holds no key");
        assert!(dir.join(CREDENTIALS_FILE).is_file());

        store_api_key(&mut settings, "a", None).unwrap();
        assert!(!has_stored_key(&settings, "a"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_environment_variable_wins_over_the_stored_key() {
        let mut settings = Settings::in_memory();
        let mut profile = two().profiles.remove(0);
        store_api_key(&mut settings, &profile.id, Some("stored")).unwrap();
        assert_eq!(api_key(&settings, &profile).as_deref(), Some("stored"));

        // PATH is set wherever tests run; an unset variable falls through.
        profile.api_key_env = Some("NB_MODELS_TEST_UNSET_VARIABLE".into());
        assert_eq!(api_key(&settings, &profile).as_deref(), Some("stored"));
        profile.api_key_env = Some("PATH".into());
        assert_eq!(api_key(&settings, &profile), std::env::var("PATH").ok());
    }

    #[cfg(feature = "nanobot-llm-providers")]
    #[test]
    fn a_registered_profile_routes_its_id_to_its_wire_model() {
        let mut llm = nanobot_llm::LlmRuntime::new();
        let mut all = two();
        all.upsert(ModelProfile { id: "half".into(), ..ModelProfile::new("Half", "", "m") });
        assert_eq!(register_profiles(&mut llm, &all, &Settings::in_memory()), 2);
        assert_eq!(
            llm.resolve("b"),
            Some(nanobot_llm::ModelRoute::new("profile:b", "big"))
        );
        assert_eq!(llm.resolve("half"), None, "an incomplete profile is not routable");
    }
}
