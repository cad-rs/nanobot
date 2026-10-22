//! The `plugin.toml` manifest (design 4.8).
//!
//! A plugin is a directory holding `plugin.toml` plus its Rhai scripts. The
//! manifest is an **audit declaration**, not a sandbox: `permissions` records
//! what the plugin says it needs, and the host reports it (design NQ-4 keeps
//! the workbench-rs semantics rather than inventing finer UI capability bits).

use serde::Deserialize;

/// The host API a plugin is written against. A mismatch rejects the plugin.
pub const HOST_API_VERSION: &str = "1";

#[derive(Debug, Deserialize)]
pub struct PluginManifest {
    pub plugin: PluginInfo,
    pub compatibility: Option<Compatibility>,
    pub permissions: Option<Permissions>,
}

#[derive(Debug, Deserialize)]
pub struct PluginInfo {
    pub id: String,
    pub name: String,
    pub version: String,
    pub api_version: String,
    pub entry_point: String,
    #[serde(default)]
    pub modules: Vec<String>,
}

/// Which products the plugin claims to work with. `"*"` means any.
#[derive(Debug, Deserialize)]
pub struct Compatibility {
    pub products: Option<Vec<String>>,
}

/// Declared needs. Every field is optional; absence means "not declared".
#[derive(Debug, Deserialize, Default)]
pub struct Permissions {
    pub document_read: Option<bool>,
    pub document_write: Option<bool>,
    pub filesystem_read: Option<bool>,
    pub network: Option<bool>,
}

/// Parses the manifest text. Syntactic errors only; [`validate`] covers the
/// semantic rules.
pub fn parse(text: &str) -> Result<PluginManifest, String> {
    toml::from_str(text).map_err(|e| e.to_string())
}

/// Semantic validation: identity, API version, entry point and module names.
pub fn validate(manifest: &PluginManifest) -> Result<(), String> {
    let info = &manifest.plugin;
    if info.id.is_empty() {
        return Err("plugin id must not be empty".to_string());
    }
    if !info
        .id
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'))
    {
        return Err(format!(
            "plugin id `{}` may only contain ASCII letters, digits, '.', '-' and '_'",
            info.id
        ));
    }
    if info.api_version != HOST_API_VERSION {
        return Err(format!(
            "API version mismatch: plugin declares `{}`, host speaks `{HOST_API_VERSION}`",
            info.api_version
        ));
    }
    if !info.entry_point.ends_with(".rhai") {
        return Err(format!(
            "entry_point `{}` must be a .rhai script",
            info.entry_point
        ));
    }
    for module in &info.modules {
        if module.is_empty() {
            return Err("module names must not be empty".to_string());
        }
        if !module
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'))
        {
            return Err(format!(
                "module name `{module}` may only contain ASCII letters, digits, '.', '-' and '_'"
            ));
        }
    }
    Ok(())
}

/// Splits `plugin.rhai:register` into `("plugin.rhai", "register")`; a bare
/// entry point defaults to the `register` function.
pub fn split_entry_point(entry: &str) -> (String, String) {
    match entry.split_once(':') {
        Some((file, func)) => (file.to_string(), func.to_string()),
        None => (entry.to_string(), "register".to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn minimal() -> String {
        r#"
[plugin]
id = "com.example.demo"
name = "Demo"
version = "0.1.0"
api_version = "1"
entry_point = "plugin.rhai"
"#
        .to_string()
    }

    #[test]
    fn parses_the_minimal_manifest() {
        let manifest = parse(&minimal()).expect("minimal manifest parses");
        assert_eq!(manifest.plugin.id, "com.example.demo");
        assert!(manifest.plugin.modules.is_empty());
        assert!(validate(&manifest).is_ok());
    }

    #[test]
    fn rejects_a_foreign_api_version() {
        let text = minimal().replace("api_version = \"1\"", "api_version = \"2\"");
        let manifest = parse(&text).expect("still parses");
        assert!(validate(&manifest).is_err());
    }

    #[test]
    fn rejects_a_bad_id_charset_and_a_non_script_entry_point() {
        let mut manifest = parse(&minimal()).expect("parses");
        manifest.plugin.id = "com example/demo".to_string();
        assert!(validate(&manifest).is_err());
        manifest.plugin.id = "com.example.demo".to_string();
        manifest.plugin.entry_point = "main.rs".to_string();
        assert!(validate(&manifest).is_err());
    }

    #[test]
    fn splits_an_optional_function_suffix() {
        assert_eq!(
            split_entry_point("plugin.rhai:register"),
            ("plugin.rhai".to_string(), "register".to_string())
        );
        assert_eq!(
            split_entry_point("plugin.rhai"),
            ("plugin.rhai".to_string(), "register".to_string())
        );
    }

    #[test]
    fn permissions_default_to_not_declared() {
        let permissions = Permissions::default();
        assert!(permissions.document_read.is_none());
        assert!(permissions.network.is_none());
    }
}
