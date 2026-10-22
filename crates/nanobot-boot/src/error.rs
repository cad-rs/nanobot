//! Error type of the boot layer (the project container + harness assembly).

use std::path::PathBuf;

/// Everything the boot layer can fail with. A missing or mismatching
/// manifest checksum is deliberately **not** an error (it is state —
/// [`crate::ChecksumState`]).
#[derive(Debug, thiserror::Error)]
pub enum BootError {
    #[error("no project manifest ({manifest}) under {dir}")]
    ManifestNotFound {
        dir: PathBuf,
        manifest: String,
    },

    #[error("io error at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("manifest parse error: {0}")]
    Manifest(#[from] toml::de::Error),

    #[error(
        "manifest format version {found} is newer than this build supports ({supported}); \
         update the product or open an older checkout"
    )]
    UnsupportedVersion { found: u32, supported: u32 },

    #[error("invalid manifest: {0}")]
    Invalid(String),

    #[error("manifest serialization failed: {0}")]
    Serialize(String),

    #[error("harness assembly failed: {0}")]
    Assembly(String),
}
