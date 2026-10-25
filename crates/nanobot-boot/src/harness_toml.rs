//! `harness.toml` discovery and loading — the harness's identity descriptor
//! (the same role `product.toml` plays for workbench-rs).
//!
//! A product ships a `harness.toml` describing its identity and defaults.
//! The standard search order is: next to the executable, then the working
//! directory. Products with a different layout (or dev builds running from
//! `target/debug`) pass their own directories to [`load_from_dirs`].
//!
//! A missing or malformed `harness.toml` is **never fatal**: the caller keeps
//! the builder's defaults (parse problems are reported on stderr so a
//! misconfigured build is visible without a debugger).
//!
//! ```toml
//! [harness]
//! id = "com.rcad.ocad"
//! name = "ocad"
//! version = "0.1.0"              # default: this crate's CARGO_PKG_VERSION
//! backend = "gpui"               # diagnostic label
//!
//! [defaults]
//! locale = "zh-CN"
//! approval = "auto"              # auto | deny-all | ui
//!
//! [provider]                     # real inference; default is ScriptedAdapter
//! kind = "openai-compatible"
//! endpoint = "https://..."
//! model = "gpt-6"
//! ```

use std::path::PathBuf;

use serde::Deserialize;

/// Which approval gate the harness assembles by default.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ApprovalMode {
    /// Tools run without asking (headless / CI).
    #[default]
    Auto,
    /// Every tool call is refused (locked-down deployments).
    DenyAll,
    /// Every tool call routes to the UI callback.
    Ui,
}

impl ApprovalMode {
    /// Parses the `approval` field ("auto" | "deny-all" | "ui").
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "auto" => Some(ApprovalMode::Auto),
            "deny-all" => Some(ApprovalMode::DenyAll),
            "ui" => Some(ApprovalMode::Ui),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            ApprovalMode::Auto => "auto",
            ApprovalMode::DenyAll => "deny-all",
            ApprovalMode::Ui => "ui",
        }
    }
}

/// Provider descriptor from the optional `[provider]` section.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderConfig {
    /// Only `openai-compatible` is built in today.
    pub kind: String,
    pub endpoint: String,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub api_key: Option<String>,
}

/// The parsed identity: what [`HarnessBuilder`](crate::HarnessBuilder) folds
/// into the assembly defaults.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HarnessProfile {
    pub id: String,
    pub name: String,
    /// Defaults to this crate's package version when not declared.
    pub version: String,
    /// Diagnostic label (which shell backend the product runs).
    pub backend: String,
    pub locale: String,
    pub approval: ApprovalMode,
    pub provider: Option<ProviderConfig>,
}

impl HarnessProfile {
    /// The built-in fallback identity (used when no harness.toml is found).
    pub fn new(id: &str, name: &str) -> Self {
        Self {
            id: id.to_string(),
            name: name.to_string(),
            version: env!("CARGO_PKG_VERSION").to_string(),
            backend: String::new(),
            locale: String::new(),
            approval: ApprovalMode::Auto,
            provider: None,
        }
    }
}

impl Default for HarnessProfile {
    fn default() -> Self {
        Self::new("unnamed.harness", "harness")
    }
}

/// The `[harness]` section as written on disk (all fields optional so a
/// partial file still parses).
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct HarnessSection {
    id: Option<String>,
    name: Option<String>,
    version: Option<String>,
    backend: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct DefaultsSection {
    locale: Option<String>,
    approval: Option<String>,
}

/// The whole `harness.toml` file as written on disk (both sections optional).
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct HarnessToml {
    harness: HarnessSection,
    defaults: DefaultsSection,
    provider: Option<ProviderConfig>,
}

impl HarnessToml {
    /// Folds the on-disk sections over `base` (the builder's defaults).
    fn into_profile(self, base: HarnessProfile) -> Result<HarnessProfile, String> {
        let mut profile = base;
        if let Some(id) = self.harness.id {
            profile.id = id;
        }
        if let Some(name) = self.harness.name {
            profile.name = name;
        }
        if let Some(version) = self.harness.version {
            profile.version = version;
        }
        if let Some(backend) = self.harness.backend {
            profile.backend = backend;
        }
        if let Some(locale) = self.defaults.locale {
            profile.locale = locale;
        }
        if let Some(approval) = &self.defaults.approval {
            profile.approval = ApprovalMode::parse(approval)
                .ok_or_else(|| format!("unknown approval mode `{approval}` (auto | deny-all | ui)"))?;
        }
        profile.provider = self.provider;
        Ok(profile)
    }
}

/// Conventional identity descriptor file name.
pub const HARNESS_TOML: &str = "harness.toml";

/// Standard search locations: the executable's directory, then the working
/// directory. Products running from `target/debug` (or with a different
/// layout) append their own directories before calling [`load_from_dirs`].
pub fn default_search_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            dirs.push(dir.to_path_buf());
        }
    }
    if let Ok(cwd) = std::env::current_dir() {
        dirs.push(cwd);
    }
    dirs
}

/// Loads the first valid `harness.toml` found in `dirs` (search order =
/// directory order). A malformed file in one directory falls through to the
/// next. Returns the profile together with the file it came from.
pub fn load_from_dirs(dirs: &[PathBuf]) -> Option<(HarnessProfile, PathBuf)> {
    for dir in dirs {
        let path = dir.join(HARNESS_TOML);
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        let toml: HarnessToml = match toml::from_str(&text) {
            Ok(toml) => toml,
            Err(e) => {
                eprintln!("harness: failed to parse {}: {e}", path.display());
                continue;
            }
        };
        match toml.into_profile(HarnessProfile::default()) {
            Ok(profile) => return Some((profile, path)),
            Err(e) => eprintln!("harness: invalid {}: {e}", path.display()),
        }
    }
    None
}

/// Convenience: [`default_search_dirs`] + [`load_from_dirs`].
pub fn discover() -> Option<(HarnessProfile, PathBuf)> {
    load_from_dirs(&default_search_dirs())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_full_harness_toml() {
        let dir = std::env::temp_dir().join(format!("nb-harness-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join(HARNESS_TOML),
            r#"
                [harness]
                id = "com.rcad.ocad"
                name = "ocad"
                version = "0.1.0"
                backend = "gpui"

                [defaults]
                locale = "zh-CN"
                approval = "deny-all"

                [provider]
                kind = "openai-compatible"
                endpoint = "https://api.example.com/v1"
                model = "gpt-6"
            "#,
        )
        .unwrap();

        let (profile, found) = load_from_dirs(std::slice::from_ref(&dir)).expect("profile loads");
        assert_eq!(found, dir.join(HARNESS_TOML));
        assert_eq!(profile.id, "com.rcad.ocad");
        assert_eq!(profile.name, "ocad");
        assert_eq!(profile.version, "0.1.0");
        assert_eq!(profile.backend, "gpui");
        assert_eq!(profile.locale, "zh-CN");
        assert_eq!(profile.approval, ApprovalMode::DenyAll);
        let provider = profile.provider.expect("provider section");
        assert_eq!(provider.kind, "openai-compatible");
        assert_eq!(provider.endpoint, "https://api.example.com/v1");
        assert_eq!(provider.model.as_deref(), Some("gpt-6"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn partial_file_fills_only_what_it_declares() {
        let dir = std::env::temp_dir().join(format!("nb-harness-partial-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join(HARNESS_TOML),
            "[harness]\nid = \"com.acme.cad\"\nname = \"Acme\"\n",
        )
        .unwrap();

        let (profile, _) = load_from_dirs(std::slice::from_ref(&dir)).expect("profile loads");
        assert_eq!(profile.id, "com.acme.cad");
        assert_eq!(profile.name, "Acme");
        // Everything else keeps the built-in defaults.
        assert_eq!(profile.version, env!("CARGO_PKG_VERSION"));
        assert_eq!(profile.approval, ApprovalMode::Auto);
        assert!(profile.provider.is_none());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn malformed_file_falls_through_to_next_dir() {
        let bad = std::env::temp_dir().join(format!("nb-harness-bad-{}", std::process::id()));
        let good = std::env::temp_dir().join(format!("nb-harness-good-{}", std::process::id()));
        std::fs::create_dir_all(&bad).unwrap();
        std::fs::create_dir_all(&good).unwrap();
        std::fs::write(bad.join(HARNESS_TOML), "not [ valid toml =").unwrap();
        std::fs::write(
            good.join(HARNESS_TOML),
            "[harness]\nid = \"com.acme.ok\"\nname = \"Ok\"\n",
        )
        .unwrap();

        let (profile, found) = load_from_dirs(&[bad.clone(), good.clone()]).expect("recovers");
        assert_eq!(profile.id, "com.acme.ok");
        assert_eq!(found, good.join(HARNESS_TOML));

        let _ = std::fs::remove_dir_all(&bad);
        let _ = std::fs::remove_dir_all(&good);
    }

    #[test]
    fn unknown_approval_mode_is_rejected_not_fatal() {
        let dir = std::env::temp_dir().join(format!("nb-harness-approval-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join(HARNESS_TOML),
            "[defaults]\napproval = \"yolo\"\n",
        )
        .unwrap();
        assert!(load_from_dirs(std::slice::from_ref(&dir)).is_none());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn missing_everywhere_returns_none() {
        let empty = std::env::temp_dir().join(format!("nb-harness-none-{}", std::process::id()));
        assert!(load_from_dirs(&[empty]).is_none());
    }
}
