//! The generic project container: directory layout, session index, and the
//! checksummed manifest machinery (extracted from the ocad project layer —
//! the CAD-specific manifest fields stayed with the product).
//!
//! A **project** is the container of one design effort: the member-document
//! list, external references, skill / knowledge package selection, session
//! index, and product extension fields. On disk it is a directory: a root
//! manifest plus a state directory ([`ProjectPaths`] documents the layout).
//! The neutral names are `project.toml` + `.harness/`; a product pins its own
//! file names through [`ProjectPaths::named`] (ocad keeps `project.ocad.toml`
//! + `.ocad/`, for example).
//!
//! The manifest type is the product's: it implements [`Manifest`] (validation,
//! checksum access, the session index, project info) and the container
//! ([`Project`]) gives it open / create / save / session-index bookkeeping
//! with the canonical-CRC32 sealing discipline. The checksum is a *state*,
//! not a gate: a hand-edited or locally modified manifest still opens, it
//! just reports [`ChecksumState::Mismatch`].

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::canonical::{crc32, to_canonical_json_string};
use crate::error::BootError;

/// The neutral default manifest file name. Products override through
/// [`ProjectPaths::named`].
pub const MANIFEST_FILE: &str = "project.toml";

/// The neutral default state-directory name. Products override through
/// [`ProjectPaths::named`].
pub const STATE_DIR: &str = ".harness";

/// Paths inside one project directory. Pure path arithmetic — no I/O, so the
/// layout is testable and the web (OPFS) form can reuse the same semantics.
///
/// Layout:
///
/// ```text
/// mybracket/                        # the project IS the working directory
/// ├── project.toml                  # manifest (product-overridable name)
/// ├── member documents              # product's own file kinds
/// ├── refs/                         # external source files
/// └── .harness/                     # harness state (product-overridable name)
///     ├── conversations/<id>/       # design-session event logs
///     └── audit/*.jsonl             # tool-call audit side logs
/// ```
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProjectPaths {
    root: PathBuf,
    manifest_file: String,
    state_dir: String,
}

impl ProjectPaths {
    /// The neutral layout (`project.toml` + `.harness/`).
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self::named(root, MANIFEST_FILE, STATE_DIR)
    }

    /// A product-flavored layout: the manifest file name and the state
    /// directory name are the product's to pin (ocad keeps
    /// `project.ocad.toml` + `.ocad/`, for example).
    pub fn named(
        root: impl Into<PathBuf>,
        manifest_file: &str,
        state_dir: &str,
    ) -> Self {
        Self {
            root: root.into(),
            manifest_file: manifest_file.to_string(),
            state_dir: state_dir.to_string(),
        }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The manifest file.
    pub fn manifest(&self) -> PathBuf {
        self.root.join(&self.manifest_file)
    }

    /// The manifest file name this layout was configured with.
    pub fn manifest_file(&self) -> &str {
        &self.manifest_file
    }

    /// The harness state directory.
    pub fn state_dir(&self) -> PathBuf {
        self.root.join(&self.state_dir)
    }

    /// The state directory name this layout was configured with.
    pub fn state_dir_name(&self) -> &str {
        &self.state_dir
    }

    /// `conversations/` — one directory per design session.
    pub fn conversations(&self) -> PathBuf {
        self.state_dir().join("conversations")
    }

    /// `conversations/<id>/` (the session's event log + audit + markers).
    pub fn conversation(&self, id: &str) -> PathBuf {
        self.conversations().join(id)
    }

    /// `audit/` — the tool-call audit side logs.
    pub fn audit(&self) -> PathBuf {
        self.state_dir().join("audit")
    }

    /// `refs/` — external source files referenced by member documents.
    pub fn refs(&self) -> PathBuf {
        self.root.join("refs")
    }

    /// Resolves a manifest-relative path, rejecting anything that would leave
    /// the project root (the manifest invariant, enforced again at use time).
    pub fn resolve(&self, relative: &str) -> Result<PathBuf, BootError> {
        validate_relative_path(relative)
            .map_err(BootError::Invalid)
            .map(|()| self.root.join(relative))
    }
}

/// Accepts a path that stays inside the project root: relative, no `..`, no
/// root/prefix component (the manifest carries **only** relative paths, which
/// is what keeps a project directory movable).
pub fn validate_relative_path(path: &str) -> Result<(), String> {
    if path.trim().is_empty() {
        return Err("path is empty".into());
    }
    let candidate = Path::new(path);
    if candidate.is_absolute() {
        return Err("path must be relative to the project root".into());
    }
    for component in candidate.components() {
        match component {
            std::path::Component::Normal(_) => {}
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                return Err("path must not escape the project root (`..`)".into())
            }
            std::path::Component::RootDir | std::path::Component::Prefix(_) => {
                return Err("path must be relative to the project root".into())
            }
        }
    }
    Ok(())
}

/// Whether the checksum carried by a manifest matched its content.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChecksumState {
    /// Present and matching.
    Verified,
    /// No checksum in the file (hand-written manifest).
    Missing,
    /// Present but not matching — the file was edited outside the product.
    Mismatch { expected: u32, found: u32 },
}

impl ChecksumState {
    pub fn is_verified(self) -> bool {
        matches!(self, ChecksumState::Verified)
    }

    pub fn label(self) -> &'static str {
        match self {
            ChecksumState::Verified => "checksum verified",
            ChecksumState::Missing => "no checksum",
            ChecksumState::Mismatch { .. } => "checksum mismatch (file edited outside the app)",
        }
    }
}

/// Project identity: name + uuid + the manifest's own revision counter.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectInfo {
    pub name: String,
    /// Stable across renames and moves — the identity the manifest carries.
    pub uuid: String,
    /// Revision counter of the manifest itself (bumped on every save).
    #[serde(default)]
    pub revision: u64,
}

impl ProjectInfo {
    pub fn new(name: &str) -> Self {
        Self {
            name: name.to_string(),
            uuid: uuid::Uuid::new_v4().to_string(),
            revision: 0,
        }
    }
}

/// A package reference with an optional version constraint (skill / knowledge
/// package selection — the manifest's profile-patch layer).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PackageRef {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
}

/// Skill / knowledge package selection.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct PackageSelection {
    pub skills: Vec<PackageRef>,
    pub knowledge: Vec<PackageRef>,
}

/// Lifecycle status of a session, as the manifest records it (matches the
/// shell's four states; the shell owns the UI wording).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionStatus {
    Active,
    Running,
    AwaitingReview,
    Archived,
}

/// One entry of the session index.
///
/// The event log itself lives in `conversations/<id>/` and belongs to
/// `nanobot-session`; this entry is what lets the session list exist without
/// replaying every log — and what the project layer persists when the user
/// switches sessions.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionEntry {
    /// The conversation directory name under `conversations/`.
    pub id: String,
    pub title: String,
    pub status: SessionStatus,
    /// Member document this session operates (one session, one document in
    /// the MVP).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub document: Option<String>,
}

/// The session index: the active session plus the listed entries.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct SessionIndex {
    /// The session the workspace presents when the project is opened.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active: Option<String>,
    pub entries: Vec<SessionEntry>,
}

impl SessionIndex {
    pub fn entry(&self, id: &str) -> Option<&SessionEntry> {
        self.entries.iter().find(|e| e.id == id)
    }

    pub fn entry_mut(&mut self, id: &str) -> Option<&mut SessionEntry> {
        self.entries.iter_mut().find(|e| e.id == id)
    }
}

/// The contract a product manifest implements so the generic container can
/// open / save / seal it. The framework never touches the product's own
/// fields — it only needs the identity block, the session index, and the
/// checksum slot.
pub trait Manifest: Clone + Serialize + serde::de::DeserializeOwned {
    /// The manifest's invariants (relative paths, unique ids, session
    /// references — whatever the product promises).
    fn validate(&self) -> Result<(), BootError>;

    /// The stored checksum (`None` = hand-written manifest).
    fn checksum(&self) -> Option<u32>;

    /// Stores or clears the checksum slot.
    fn set_checksum(&mut self, checksum: Option<u32>);

    /// The session index (read).
    fn sessions(&self) -> &SessionIndex;

    /// The session index (write).
    fn sessions_mut(&mut self) -> &mut SessionIndex;

    /// The identity block (read).
    fn project_info(&self) -> &ProjectInfo;

    /// The identity block (write).
    fn project_info_mut(&mut self) -> &mut ProjectInfo;

    /// Parses a manifest. The provided impl validates after parsing; a
    /// product overrides it to add its own gates (a format-version ceiling,
    /// for example).
    fn from_toml_string(text: &str) -> Result<Self, BootError> {
        let manifest: Self = toml::from_str(text)?;
        manifest.validate()?;
        Ok(manifest)
    }

    /// CRC32 over the canonical JSON of the manifest **with the checksum
    /// cleared** — what [`Manifest::seal`] writes and
    /// [`Manifest::checksum_state`] compares against.
    fn content_crc32(&self) -> Result<u32, BootError> {
        let mut bare = self.clone();
        bare.set_checksum(None);
        Ok(crc32(to_canonical_json_string(&bare)?.as_bytes()))
    }

    /// Recomputes and stores the checksum (called on every save, so files
    /// written through [`Project::save`] are always self-consistent).
    fn seal(&mut self) -> Result<(), BootError> {
        self.set_checksum(Some(self.content_crc32()?));
        Ok(())
    }

    /// Whether the file's checksum matches its content.
    fn checksum_state(&self) -> Result<ChecksumState, BootError> {
        match self.checksum() {
            None => Ok(ChecksumState::Missing),
            Some(found) => {
                let expected = self.content_crc32()?;
                Ok(if expected == found {
                    ChecksumState::Verified
                } else {
                    ChecksumState::Mismatch { expected, found }
                })
            }
        }
    }

    /// Serializes to TOML in a fixed field order (canonical by construction:
    /// struct field order, ordered collections, no maps iterated in hash
    /// order). The checksum is always recomputed first.
    fn to_toml_string(&self) -> Result<String, BootError> {
        let mut sealed = self.clone();
        sealed.seal()?;
        toml::to_string(&sealed).map_err(|e| BootError::Serialize(format!("manifest -> toml: {e}")))
    }
}

/// An open project: its directory, manifest and checksum state, generic over
/// the product's manifest type.
#[derive(Clone, Debug)]
pub struct Project<M: Manifest> {
    paths: ProjectPaths,
    manifest: M,
    checksum: ChecksumState,
}

impl<M: Manifest> Project<M> {
    /// Creates a project directory at `paths`: the manifest plus the state
    /// directories. The root may exist (an empty or foreign directory) but
    /// must not already contain a manifest — reopening is [`Project::open`].
    pub fn create_at(paths: &ProjectPaths, manifest: M) -> Result<Self, BootError> {
        std::fs::create_dir_all(paths.root()).map_err(|source| BootError::Io {
            path: paths.root().to_path_buf(),
            source,
        })?;
        if paths.manifest().is_file() {
            return Err(BootError::Invalid(format!(
                "{} already contains {}",
                paths.root().display(),
                paths.manifest_file()
            )));
        }
        let mut project = Self {
            paths: paths.clone(),
            manifest,
            checksum: ChecksumState::Verified,
        };
        project.create_state_dirs()?;
        project.save()?;
        Ok(project)
    }

    /// [`Project::create_at`] on the neutral layout.
    pub fn create(root: impl Into<PathBuf>, manifest: M) -> Result<Self, BootError> {
        Self::create_at(&ProjectPaths::new(root), manifest)
    }

    /// Opens the project whose manifest lives at `paths`.
    pub fn open_at(paths: &ProjectPaths) -> Result<Self, BootError> {
        let manifest_path = paths.manifest();
        let text = match std::fs::read_to_string(&manifest_path) {
            Ok(text) => text,
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => {
                return Err(BootError::ManifestNotFound {
                    dir: paths.root().to_path_buf(),
                    manifest: paths.manifest_file().to_string(),
                })
            }
            Err(source) => {
                return Err(BootError::Io {
                    path: manifest_path,
                    source,
                })
            }
        };
        let manifest = M::from_toml_string(&text)?;
        let checksum = manifest.checksum_state()?;
        Ok(Self {
            paths: paths.clone(),
            manifest,
            checksum,
        })
    }

    /// [`Project::open_at`] on the neutral layout.
    pub fn open(root: impl Into<PathBuf>) -> Result<Self, BootError> {
        Self::open_at(&ProjectPaths::new(root))
    }

    pub fn paths(&self) -> &ProjectPaths {
        &self.paths
    }

    pub fn root(&self) -> &Path {
        self.paths.root()
    }

    pub fn manifest(&self) -> &M {
        &self.manifest
    }

    pub fn manifest_mut(&mut self) -> &mut M {
        &mut self.manifest
    }

    /// Whether the file's checksum matched on load (`Mismatch` means the
    /// manifest was edited outside the product; the state survives until the
    /// next save re-seals it).
    pub fn checksum_state(&self) -> ChecksumState {
        self.checksum
    }

    /// Writes the manifest back (checksum re-sealed, revision bumped).
    pub fn save(&mut self) -> Result<(), BootError> {
        self.manifest.project_info_mut().revision =
            self.manifest.project_info_mut().revision.saturating_add(1);
        self.manifest.seal()?;
        let text = self.manifest.to_toml_string()?;
        let path = self.paths.manifest();
        std::fs::write(&path, text).map_err(|source| BootError::Io { path, source })?;
        self.checksum = ChecksumState::Verified;
        Ok(())
    }

    /// Makes the state directories exist (idempotent).
    pub fn create_state_dirs(&self) -> Result<(), BootError> {
        for dir in [self.paths.conversations(), self.paths.audit()] {
            std::fs::create_dir_all(&dir).map_err(|source| BootError::Io { path: dir, source })?;
        }
        Ok(())
    }

    /// The session's conversation directory, created on demand.
    pub fn ensure_conversation_dir(&self, id: &str) -> Result<PathBuf, BootError> {
        self.manifest.sessions().entry(id).ok_or_else(|| {
            BootError::Invalid(format!("session `{id}` is not in the session index"))
        })?;
        let dir = self.paths.conversation(id);
        std::fs::create_dir_all(&dir).map_err(|source| BootError::Io {
            path: dir.clone(),
            source,
        })?;
        Ok(dir)
    }

    /// The conversation directories actually present on disk (sorted). The
    /// session index is authoritative; this is what reconciles against it.
    pub fn conversation_dir_ids(&self) -> Result<Vec<String>, BootError> {
        let root = self.paths.conversations();
        let entries = match std::fs::read_dir(&root) {
            Ok(entries) => entries,
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(source) => return Err(BootError::Io { path: root, source }),
        };
        let mut ids = Vec::new();
        for entry in entries {
            let entry = entry.map_err(|source| BootError::Io {
                path: root.clone(),
                source,
            })?;
            if entry.path().is_dir() {
                ids.push(entry.file_name().to_string_lossy().into_owned());
            }
        }
        ids.sort();
        Ok(ids)
    }

    /// Records a session the project should list (create-or-replace by id) and
    /// makes its conversation directory exist.
    pub fn upsert_session(&mut self, entry: SessionEntry) -> Result<(), BootError> {
        self.create_state_dirs()?;
        match self.manifest.sessions_mut().entry_mut(&entry.id) {
            Some(existing) => *existing = entry.clone(),
            None => self.manifest.sessions_mut().entries.push(entry.clone()),
        }
        if self.manifest.sessions().active.is_none() {
            self.manifest.sessions_mut().active = Some(entry.id.clone());
        }
        self.manifest.validate()?;
        self.ensure_conversation_dir(&entry.id)?;
        self.save()
    }

    /// Sets the active session — what the shell calls when the user switches
    /// sessions, so reopening the project restores it.
    pub fn set_active_session(&mut self, id: &str) -> Result<(), BootError> {
        if self.manifest.sessions().entry(id).is_none() {
            return Err(BootError::Invalid(format!(
                "session `{id}` is not in the session index"
            )));
        }
        self.manifest.sessions_mut().active = Some(id.to_string());
        self.save()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A minimal product manifest for the container tests: identity +
    /// documents (neutral `kind: String`) + packages + sessions.
    #[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
    struct TestManifest {
        pub format_version: u32,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub checksum: Option<u32>,
        pub project: ProjectInfo,
        #[serde(default, rename = "document")]
        pub documents: Vec<TestDocument>,
        #[serde(default)]
        pub packages: PackageSelection,
        #[serde(default)]
        pub sessions: SessionIndex,
    }

    #[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
    struct TestDocument {
        pub id: String,
        pub path: String,
        pub kind: String,
    }

    impl TestManifest {
        fn new(name: &str) -> Self {
            Self {
                format_version: 1,
                checksum: None,
                project: ProjectInfo::new(name),
                documents: Vec::new(),
                packages: PackageSelection::default(),
                sessions: SessionIndex::default(),
            }
        }
    }

    impl Manifest for TestManifest {
        fn validate(&self) -> Result<(), BootError> {
            if self.project.name.trim().is_empty() {
                return Err(BootError::Invalid("project name is empty".into()));
            }
            for document in &self.documents {
                validate_relative_path(&document.path)
                    .map_err(|e| BootError::Invalid(format!("document `{}`: {e}", document.id)))?;
            }
            Ok(())
        }

        fn checksum(&self) -> Option<u32> {
            self.checksum
        }

        fn set_checksum(&mut self, checksum: Option<u32>) {
            self.checksum = checksum;
        }

        fn sessions(&self) -> &SessionIndex {
            &self.sessions
        }

        fn sessions_mut(&mut self) -> &mut SessionIndex {
            &mut self.sessions
        }

        fn project_info(&self) -> &ProjectInfo {
            &self.project
        }

        fn project_info_mut(&mut self) -> &mut ProjectInfo {
            &mut self.project
        }
    }

    #[test]
    fn create_save_reopen_round_trips_the_manifest() {
        let root = std::env::temp_dir().join(format!("nb-boot-container-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);

        let mut project = Project::<TestManifest>::create(&root, TestManifest::new("bracket"))
            .expect("create");
        assert!(root.join(MANIFEST_FILE).is_file());
        assert!(root.join(STATE_DIR).join("conversations").is_dir());
        assert!(root.join(STATE_DIR).join("audit").is_dir());
        let uuid = project.manifest().project.uuid.clone();

        project
            .upsert_session(SessionEntry {
                id: "s-1".into(),
                title: "first".into(),
                status: SessionStatus::Active,
                document: None,
            })
            .expect("upsert");
        project.set_active_session("s-1").expect("activate");

        let reopened = Project::<TestManifest>::open(&root).expect("reopen");
        assert_eq!(reopened.manifest().project.uuid, uuid);
        assert_eq!(reopened.manifest().sessions.active.as_deref(), Some("s-1"));
        assert_eq!(reopened.manifest().sessions.entries.len(), 1);
        assert_eq!(reopened.checksum_state(), ChecksumState::Verified);

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn hand_edited_manifest_still_opens_and_reports_mismatch() {
        let root = std::env::temp_dir().join(format!("nb-boot-mismatch-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let project =
            Project::<TestManifest>::create(&root, TestManifest::new("hand edit")).expect("create");

        let text = std::fs::read_to_string(project.paths().manifest()).unwrap();
        let edited = text.replace("hand edit", "hand edit (renamed)");
        std::fs::write(project.paths().manifest(), edited).unwrap();

        let reopened = Project::<TestManifest>::open(&root).expect("still opens");
        assert!(matches!(
            reopened.checksum_state(),
            ChecksumState::Mismatch { .. }
        ));
        assert_eq!(reopened.manifest().project.name, "hand edit (renamed)");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn paths_reject_escape_and_accept_inner_paths() {
        let paths = ProjectPaths::new("C:/projects/bracket");
        assert!(paths.resolve("refs/motor.step").is_ok());
        assert!(paths.resolve("/abs.step").is_err());
        assert!(paths.resolve("a/../../b.step").is_err());

        let named = ProjectPaths::named("C:/projects/bracket", "project.ocad.toml", ".ocad");
        assert_eq!(
            named.manifest(),
            std::path::Path::new("C:/projects/bracket").join("project.ocad.toml")
        );
        assert_eq!(named.state_dir_name(), ".ocad");
    }

    #[test]
    fn toml_round_trip_is_stable_and_sealed() {
        let manifest = TestManifest::new("stable");
        let first = manifest.to_toml_string().unwrap();
        let second = manifest.to_toml_string().unwrap();
        assert_eq!(first, second, "canonical serialization is deterministic");
        let parsed = TestManifest::from_toml_string(&first).unwrap();
        assert_eq!(parsed.checksum_state().unwrap(), ChecksumState::Verified);
    }
}
