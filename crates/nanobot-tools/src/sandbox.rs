//! Sandbox policy: what a tool may touch outside the workspace.
//!
//! A harness that runs external tools (a solver, a simulator, a codegen step)
//! needs to say what those tools may do before they do it — not after. This
//! module is the policy half: a [`SandboxMode`] plus a [`PathSandbox`] that
//! answers "may this write here?" and "may this reach the network?".
//!
//! Enforcement is the host's job (a real sandbox needs OS support — seatbelt,
//! bubblewrap, a container). The policy is what the host enforces *against*, so
//! a tool can be refused before it starts rather than killed mid-run.

use std::path::Path;

/// Glob match supporting `*` (any run of characters) and `?` (one character).
fn glob_match(pattern: &str, text: &str) -> bool {
    let p: Vec<char> = pattern.chars().collect();
    let t: Vec<char> = text.chars().collect();
    let (mut pi, mut ti) = (0_usize, 0_usize);
    let mut star: Option<usize> = None;
    let mut star_ti = 0_usize;

    while ti < t.len() {
        if pi < p.len() && (p[pi] == '?' || p[pi] == t[ti]) {
            pi += 1;
            ti += 1;
        } else if pi < p.len() && p[pi] == '*' {
            star = Some(pi);
            star_ti = ti;
            pi += 1;
        } else if let Some(star_pi) = star {
            pi = star_pi + 1;
            star_ti += 1;
            ti = star_ti;
        } else {
            return false;
        }
    }
    while pi < p.len() && p[pi] == '*' {
        pi += 1;
    }
    pi == p.len()
}

/// How much a sandboxed tool may touch.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SandboxMode {
    /// No restriction at all. Appropriate only for a fully trusted tool.
    #[default]
    None,
    /// Read anywhere, write nowhere.
    ReadOnly,
    /// Read anywhere, write only inside the workspace roots.
    WorkspaceWrite,
    /// Read and write anywhere, but no network.
    LocalOnly,
}

impl SandboxMode {
    /// Stable label for config and logs.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::ReadOnly => "read-only",
            Self::WorkspaceWrite => "workspace-write",
            Self::LocalOnly => "local-only",
        }
    }

    /// Whether writes are permitted anywhere.
    #[must_use]
    pub fn allows_any_write(self) -> bool {
        matches!(self, Self::None)
    }

    /// Whether network access is permitted.
    #[must_use]
    pub fn allows_network(self) -> bool {
        !matches!(self, Self::LocalOnly)
    }
}

/// A decision about one operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SandboxDecision {
    /// Permitted.
    Allow,
    /// Refused; the string is the agent-readable reason.
    Deny(&'static str),
}

/// Path + network policy over a set of workspace roots.
///
/// Roots are matched by path prefix, so a tool may write under
/// `/work/project` but not `/work/other` or `/etc`.
#[derive(Debug, Clone)]
pub struct PathSandbox {
    mode: SandboxMode,
    /// Absolute-ish root prefixes writes are allowed under.
    write_roots: Vec<String>,
    /// Path globs that are never writable, even inside a root (e.g. VCS state).
    protected: Vec<String>,
    /// Host globs the tool may reach (`*` = any). Empty with a permissive mode
    /// means any.
    network_allow: Vec<String>,
}

impl PathSandbox {
    /// A sandbox in `mode` with no write roots and no network allowlist.
    #[must_use]
    pub fn new(mode: SandboxMode) -> Self {
        Self {
            mode,
            write_roots: Vec::new(),
            protected: Vec::new(),
            network_allow: Vec::new(),
        }
    }

    /// Add a writable root (a prefix).
    #[must_use]
    pub fn with_write_root(mut self, root: impl Into<String>) -> Self {
        self.write_roots.push(normalize(&root.into()));
        self
    }

    /// Add a protected path glob that is never writable.
    #[must_use]
    pub fn protecting(mut self, glob: impl Into<String>) -> Self {
        self.protected.push(glob.into());
        self
    }

    /// Add a network allowlist entry (a host glob, `*` = any).
    #[must_use]
    pub fn allowing_network(mut self, host_glob: impl Into<String>) -> Self {
        self.network_allow.push(host_glob.into());
        self
    }

    /// The mode.
    #[must_use]
    pub fn mode(&self) -> SandboxMode {
        self.mode
    }

    /// May a tool write to `path`?
    #[must_use]
    pub fn check_write(&self, path: &str) -> SandboxDecision {
        if self.mode.allows_any_write() {
            return SandboxDecision::Allow;
        }
        if matches!(self.mode, SandboxMode::ReadOnly) {
            return SandboxDecision::Deny("sandbox is read-only");
        }
        let path = normalize(path);
        if self.protected.iter().any(|glob| glob_match(glob, &path)) {
            return SandboxDecision::Deny("path is protected by the sandbox");
        }
        let under_root = self.write_roots.iter().any(|root| under(root, &path));
        if under_root {
            SandboxDecision::Allow
        } else {
            SandboxDecision::Deny("path is outside the sandbox's writable roots")
        }
    }

    /// May a tool reach `host`?
    #[must_use]
    pub fn check_network(&self, host: &str) -> SandboxDecision {
        if !self.mode.allows_network() {
            return SandboxDecision::Deny("sandbox has no network access");
        }
        if self.network_allow.is_empty() || self.network_allow.iter().any(|g| g == "*") {
            return SandboxDecision::Allow;
        }
        if self.network_allow.iter().any(|glob| glob_match(glob, host)) {
            SandboxDecision::Allow
        } else {
            SandboxDecision::Deny("host is not on the sandbox's network allowlist")
        }
    }
}

impl Default for PathSandbox {
    fn default() -> Self {
        Self::new(SandboxMode::default())
    }
}

/// Normalize a path for prefix comparison: strip `.`/`..` segments and make it
/// absolute-ish by trimming a leading `./`.
fn normalize(path: &str) -> String {
    let mut parts: Vec<&str> = Vec::new();
    for segment in Path::new(path).components() {
        match segment {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                parts.pop();
            }
            other => parts.push(other.as_os_str().to_str().unwrap_or_default()),
        }
    }
    format!("/{}", parts.join("/"))
}

/// Whether `path` is `root` itself or under it.
fn under(root: &str, path: &str) -> bool {
    path == root || path.starts_with(&format!("{root}/"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_only_writes_nothing() {
        let sandbox = PathSandbox::new(SandboxMode::ReadOnly).with_write_root("/work/project");
        assert_eq!(
            sandbox.check_write("/work/project/out.gds"),
            SandboxDecision::Deny("sandbox is read-only")
        );
        assert_eq!(
            sandbox.check_write("/etc/passwd"),
            SandboxDecision::Deny("sandbox is read-only")
        );
    }

    #[test]
    fn workspace_write_is_scoped_to_its_roots() {
        let sandbox =
            PathSandbox::new(SandboxMode::WorkspaceWrite).with_write_root("/work/project");
        assert_eq!(
            sandbox.check_write("/work/project/out.gds"),
            SandboxDecision::Allow
        );
        assert_eq!(
            sandbox.check_write("/work/other/out.gds"),
            SandboxDecision::Deny("path is outside the sandbox's writable roots")
        );
        assert_eq!(
            sandbox.check_write("/etc/passwd"),
            SandboxDecision::Deny("path is outside the sandbox's writable roots")
        );
    }

    #[test]
    fn protected_paths_are_never_writable() {
        let sandbox = PathSandbox::new(SandboxMode::WorkspaceWrite)
            .with_write_root("/work/project")
            .protecting("*/.git/*");
        assert_eq!(
            sandbox.check_write("/work/project/.git/config"),
            SandboxDecision::Deny("path is protected by the sandbox")
        );
        assert_eq!(
            sandbox.check_write("/work/project/src/main.rs"),
            SandboxDecision::Allow
        );
    }

    #[test]
    fn path_normalization_resolves_dot_segments() {
        // A `..` cannot escape the root by spelling.
        let sandbox =
            PathSandbox::new(SandboxMode::WorkspaceWrite).with_write_root("/work/project");
        assert_eq!(
            sandbox.check_write("/work/project/../../etc/passwd"),
            SandboxDecision::Deny("path is outside the sandbox's writable roots")
        );
    }

    #[test]
    fn network_is_gated_by_mode_and_allowlist() {
        let open = PathSandbox::new(SandboxMode::None);
        assert_eq!(open.check_network("any.host"), SandboxDecision::Allow);

        let local = PathSandbox::new(SandboxMode::LocalOnly);
        assert_eq!(
            local.check_network("any.host"),
            SandboxDecision::Deny("sandbox has no network access")
        );

        let allowlisted = PathSandbox::new(SandboxMode::WorkspaceWrite)
            .allowing_network("pypi.org")
            .allowing_network("*.github.com");
        assert_eq!(
            allowlisted.check_network("pypi.org"),
            SandboxDecision::Allow
        );
        assert_eq!(
            allowlisted.check_network("api.github.com"),
            SandboxDecision::Allow
        );
        assert_eq!(
            allowlisted.check_network("evil.example"),
            SandboxDecision::Deny("host is not on the sandbox's network allowlist")
        );
    }

    #[test]
    fn none_is_unrestricted() {
        let sandbox = PathSandbox::new(SandboxMode::None);
        assert!(sandbox.mode().allows_any_write());
        assert_eq!(sandbox.check_write("/anywhere"), SandboxDecision::Allow);
    }
}
