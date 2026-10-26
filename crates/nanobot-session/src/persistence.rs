//! JSONL persistence for the event log (design-harness 10.5).
//!
//! Layout: `.ocad/conversations/<session-id>/events.jsonl`
//! MVP uses full-snapshot writes (simple, validatable, diffable).
//!
//! On wasm32-unknown-unknown, `std::fs` compiles but every operation returns
//! `ErrorKind::Unsupported` at runtime. The web form should use an OPFS-backed
//! handle (design-harness 13.3). Until that abstraction lands, the wasm path
//! returns an explicit error instead of relying on std's silent stubs.

use crate::session::LogEntry;
#[cfg(not(target_family = "wasm"))]
use std::fs;
#[cfg(not(target_family = "wasm"))]
use std::io::{BufRead, BufReader, Write};
use std::path::Path;

/// Save the event log to a JSONL file.
///
/// Each entry is serialized as one JSON object per line. The file is
/// truncated and rewritten in full (MVP snapshot strategy).
#[cfg(not(target_family = "wasm"))]
pub fn save(path: &Path, entries: &[LogEntry]) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }

    let mut file = fs::File::create(path)?;
    for entry in entries {
        let line = serde_json::to_string(entry)?;
        writeln!(file, "{line}")?;
    }
    Ok(())
}

/// Map a `.ocad/...` path to the OPFS-safe form (`_ocad/...`).
///
/// OPFS rejects dot-prefixed directory names, so the state directory is
/// `_ocad` rather than `.ocad` (same convention as `ocad-engine`).
#[cfg(target_arch = "wasm32")]
fn opfs_path(path: &Path) -> String {
    let s = path.to_string_lossy();
    s.replace(".ocad", "_ocad")
}

/// Save the event log to OPFS as a JSONL file (full snapshot).
#[cfg(target_family = "wasm")]
pub async fn save_opfs(path: &Path, entries: &[LogEntry]) -> Result<(), String> {
    let content: String = entries
        .iter()
        .map(|e| serde_json::to_string(e).map_err(|err| err.to_string()))
        .collect::<Result<Vec<_>, _>>()?
        .join("\n");
    let content = if content.is_empty() {
        String::new()
    } else {
        format!("{content}\n")
    };
    opfs::write_file(&opfs_path(path), &content).await
}

/// Append one JSON value as a line to an OPFS file.
///
/// OPFS writable streams support seeking; we write at the end of the
/// existing content.
#[cfg(target_family = "wasm")]
pub async fn append_opfs(path: &Path, value: &serde_json::Value) -> Result<(), String> {
    let line = serde_json::to_string(value).map_err(|err| err.to_string())?;
    let existing = opfs::read_file(&opfs_path(path)).await.unwrap_or_default();
    let content = if existing.is_empty() {
        format!("{line}\n")
    } else if existing.ends_with('\n') {
        format!("{existing}{line}\n")
    } else {
        format!("{existing}\n{line}\n")
    };
    opfs::write_file(&opfs_path(path), &content).await
}

/// Load the event log from an OPFS JSONL file.
#[cfg(target_family = "wasm")]
pub async fn load_opfs(path: &Path) -> Result<Vec<LogEntry>, String> {
    let content = opfs::read_file(&opfs_path(path)).await?;
    let mut entries = Vec::new();
    for line in content.lines() {
        if line.trim().is_empty() {
            continue;
        }
        match serde_json::from_str(line) {
            Ok(entry) => entries.push(entry),
            Err(e) => return Err(e.to_string()),
        }
    }
    Ok(entries)
}

/// Append one JSON value as a line, creating the file and its parents.
///
/// Unlike [`save`], this never rewrites: an append-only side log (the audit
/// trail) must not lose earlier records.
#[cfg(not(target_family = "wasm"))]
pub fn append_line(path: &Path, value: &serde_json::Value) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    writeln!(file, "{}", serde_json::to_string(value)?)?;
    Ok(())
}

/// Load the event log from a JSONL file.
///
/// Each line is deserialized as one `LogEntry`. Returns an error if the
/// file cannot be read or if any line fails to parse.
#[cfg(not(target_family = "wasm"))]
pub fn load(path: &Path) -> std::io::Result<Vec<LogEntry>> {
    let file = fs::File::open(path)?;
    let reader = BufReader::new(file);
    let mut entries = Vec::new();

    for line in reader.lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let entry: LogEntry = serde_json::from_str(&line)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        entries.push(entry);
    }

    Ok(entries)
}

/// Get the default persistence path for a session ID.
///
/// Returns `.ocad/conversations/<session-id>/events.jsonl`.
pub fn default_path(session_id: &str) -> std::path::PathBuf {
    Path::new(".ocad")
        .join("conversations")
        .join(session_id)
        .join("events.jsonl")
}

/// OPFS helpers (wasm only).
#[cfg(target_family = "wasm")]
mod opfs {
    use js_sys::Reflect;
    use wasm_bindgen::{JsCast, JsValue};
    use wasm_bindgen_futures::JsFuture;
    use web_sys::{
        FileSystemDirectoryHandle, FileSystemFileHandle, FileSystemGetDirectoryOptions,
        FileSystemGetFileOptions, FileSystemWritableFileStream,
    };

    /// Resolve a directory by name, creating it when `create` is set.
    async fn resolve_dir(
        parent: &FileSystemDirectoryHandle,
        name: &str,
        create: bool,
    ) -> Option<FileSystemDirectoryHandle> {
        let options = FileSystemGetDirectoryOptions::new();
        options.set_create(create);
        let handle = if create {
            parent.get_directory_handle_with_options(name, &options)
        } else {
            parent.get_directory_handle(name)
        };
        JsFuture::from(handle).await.ok()?.dyn_into().ok()
    }

    /// Write a string to a file, creating parent directories as needed.
    pub async fn write_file(relative_path: &str, content: &str) -> Result<(), String> {
        let window = web_sys::window().ok_or("no window")?;
        let navigator = window.navigator();
        let storage = navigator.storage();
        let root = JsFuture::from(storage.get_directory())
            .await
            .map_err(|_| "get_directory failed")?
            .dyn_into::<FileSystemDirectoryHandle>()
            .map_err(|_: JsValue| "not a directory handle")?;

        let mut parts: Vec<&str> = relative_path.split('/').filter(|p| !p.is_empty()).collect();
        let file_name = parts.pop().ok_or("no file name")?;
        let mut dir = root.clone();
        for part in parts {
            dir = resolve_dir(&dir, part, true)
                .await
                .ok_or_else(|| format!("failed to create dir: {part}"))?;
        }

        let options = FileSystemGetFileOptions::new();
        options.set_create(true);
        let handle = JsFuture::from(dir.get_file_handle_with_options(file_name, &options))
            .await
            .map_err(|_| "get_file_handle failed")?
            .dyn_into::<FileSystemFileHandle>()
            .map_err(|_: JsValue| "not a file handle")?;

        let writable = JsFuture::from(handle.create_writable())
            .await
            .map_err(|_| "create_writable failed")?
            .dyn_into::<FileSystemWritableFileStream>()
            .map_err(|_: JsValue| "not a writable stream")?;

        JsFuture::from(
            writable
                .write_with_str(content)
                .map_err(|_| "write_with_str failed")?,
        )
        .await
        .map_err(|_| "write failed")?;
        JsFuture::from(writable.close())
            .await
            .map_err(|_| "close failed")?;
        Ok(())
    }

    /// Read a file as a string. Empty string when the path does not exist.
    pub async fn read_file(relative_path: &str) -> Result<String, String> {
        let window = web_sys::window().ok_or("no window")?;
        let navigator = window.navigator();
        let storage = navigator.storage();
        let root = JsFuture::from(storage.get_directory())
            .await
            .map_err(|_| "get_directory failed")?
            .dyn_into::<FileSystemDirectoryHandle>()
            .map_err(|_: JsValue| "not a directory handle")?;

        let mut parts: Vec<&str> = relative_path.split('/').filter(|p| !p.is_empty()).collect();
        let file_name = parts.pop().ok_or("no file name")?;
        let mut dir = root.clone();
        for part in parts {
            dir = resolve_dir(&dir, part, false)
                .await
                .ok_or_else(|| format!("dir not found: {part}"))?;
        }

        let handle = JsFuture::from(dir.get_file_handle(file_name))
            .await
            .map_err(|_| "get_file_handle failed")?
            .dyn_into::<FileSystemFileHandle>()
            .map_err(|_: JsValue| "not a file handle")?;

        let file = JsFuture::from(handle.get_file())
            .await
            .map_err(|_| "get_file failed")?
            .dyn_into::<web_sys::File>()
            .map_err(|_: JsValue| "not a file")?;

        JsFuture::from(file.text())
            .await
            .map_err(|_| "read failed")?
            .as_string()
            .ok_or_else(|| "text not a string".to_string())
    }

    /// Reflect import for future use (keeps the import from being flagged).
    #[allow(dead_code)]
    fn _reflect_import() {
        let _ = Reflect::get;
    }
}
