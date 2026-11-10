//! Replacing a file without putting its old content at risk (native builds).
//!
//! Every durable file the framework writes whole — the event-log snapshot,
//! the project manifest, the user settings — goes through [`write`], so a
//! crash, power cut or full disk never leaves half of one behind.

use std::fs::{self, File, OpenOptions, Permissions};
use std::io::{self, ErrorKind, Write as _};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

/// Create the file at `path`, or replace it, with `bytes`.
///
/// The new content goes to a temporary file beside it and reaches the disk
/// before that file is renamed over `path`, so a crash, power cut or full
/// disk leaves either the old content or the new, never part of one.
pub fn write(path: &Path, bytes: &[u8]) -> io::Result<()> {
    // Renaming over a symbolic link would replace the link, not its file.
    let is_link = fs::symlink_metadata(path).is_ok_and(|m| m.file_type().is_symlink());
    let target = if is_link { fs::canonicalize(path)? } else { path.to_path_buf() };
    // A rename may replace a file that can't be written (read-only, another
    // user's), so ask for what writing in place would have needed.
    let permissions = match OpenOptions::new().write(true).open(&target) {
        Ok(file) => Some(file.metadata()?.permissions()),
        Err(e) if e.kind() == ErrorKind::NotFound => None,
        Err(e) => return Err(e),
    };
    let temp = write_temp(&target, bytes, permissions)?;
    let replaced = replace(&temp, &target);
    if replaced.is_err() {
        let _ = fs::remove_file(&temp);
    }
    replaced
}

/// Write `bytes` to a new temporary file beside `target` and flush them to
/// the disk; returns its path.
fn write_temp(
    target: &Path,
    bytes: &[u8],
    permissions: Option<Permissions>,
) -> io::Result<PathBuf> {
    let (mut file, temp) = create_temp(target)?;
    let written = permissions
        .map_or(Ok(()), |permissions| file.set_permissions(permissions))
        .and_then(|()| file.write_all(bytes))
        .and_then(|()| file.sync_all());
    drop(file);
    if let Err(e) = written {
        let _ = fs::remove_file(&temp);
        return Err(e);
    }
    Ok(temp)
}

/// A new file in `target`'s folder (the same volume, or the rename could not
/// be atomic). Its name doesn't derive from `target`, which may already be as
/// long as a name can be. Only a process that dies mid-save leaves one behind.
fn create_temp(target: &Path) -> io::Result<(File, PathBuf)> {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    loop {
        let unique = NEXT.fetch_add(1, Ordering::Relaxed);
        let temp = target.with_file_name(format!(".nb-save-{}-{unique}.tmp", std::process::id()));
        match OpenOptions::new().write(true).create_new(true).open(&temp) {
            Ok(file) => return Ok((file, temp)),
            Err(e) if e.kind() == ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    }
}

/// Move the finished `temp` over `target`.
fn replace(temp: &Path, target: &Path) -> io::Result<()> {
    rename(temp, target)?;
    // The rename is durable once the folder's entry is (Windows has no such
    // step, nor a way to open a folder for it).
    #[cfg(unix)]
    if let Some(dir) = target.parent() {
        let _ = File::open(dir).and_then(|dir| dir.sync_all());
    }
    Ok(())
}

/// Windows refuses the rename while a virus scanner or indexer briefly holds
/// either file, so a refusal there is retried for a moment.
fn rename(from: &Path, to: &Path) -> io::Result<()> {
    const ERROR_ACCESS_DENIED: i32 = 5;
    const ERROR_SHARING_VIOLATION: i32 = 32;
    let mut retries = 10;
    loop {
        match fs::rename(from, to) {
            Err(e)
                if cfg!(windows)
                    && retries > 0
                    && matches!(
                        e.raw_os_error(),
                        Some(ERROR_ACCESS_DENIED | ERROR_SHARING_VIOLATION)
                    ) =>
            {
                retries -= 1;
                std::thread::sleep(Duration::from_millis(20));
            }
            result => return result,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A new empty folder for one test.
    fn empty_dir(test: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("nb-atomic-{}-{test}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn names_in(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = fs::read_dir(dir)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    #[test]
    fn creates_then_replaces_and_leaves_only_the_file() {
        let dir = empty_dir("replace");
        let file = dir.join("doc.bin");
        write(&file, b"first").unwrap();
        assert_eq!(fs::read(&file).unwrap(), b"first");
        write(&file, b"second, longer").unwrap();
        assert_eq!(fs::read(&file).unwrap(), b"second, longer");
        write(&file, b"3").unwrap();
        assert_eq!(fs::read(&file).unwrap(), b"3");
        assert_eq!(names_in(&dir), ["doc.bin"]);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_old_content_survives_until_the_final_rename() {
        let dir = empty_dir("survives");
        let file = dir.join("doc.bin");
        fs::write(&file, b"old").unwrap();

        let temp = write_temp(&file, b"new", None).unwrap();
        assert_eq!(fs::read(&file).unwrap(), b"old", "a crash here loses nothing");
        assert_eq!(fs::read(&temp).unwrap(), b"new");

        replace(&temp, &file).unwrap();
        assert_eq!(fs::read(&file).unwrap(), b"new");
        assert_eq!(names_in(&dir), ["doc.bin"]);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_file_that_cannot_be_written_is_left_alone() {
        let dir = empty_dir("read-only");
        let file = dir.join("doc.bin");
        fs::write(&file, b"keep me").unwrap();
        let writable = fs::metadata(&file).unwrap().permissions();
        let mut read_only = writable.clone();
        read_only.set_readonly(true);
        fs::set_permissions(&file, read_only).unwrap();

        assert!(write(&file, b"overwrite").is_err());
        assert_eq!(fs::read(&file).unwrap(), b"keep me");
        assert_eq!(names_in(&dir), ["doc.bin"]);

        fs::set_permissions(&file, writable).unwrap();
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_folder_in_the_way_is_refused_without_leftovers() {
        let dir = empty_dir("folder");
        let folder = dir.join("name");
        fs::create_dir(&folder).unwrap();
        assert!(write(&folder, b"x").is_err());
        assert_eq!(names_in(&dir), ["name"]);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_reader_only_ever_sees_a_whole_file() {
        const SIZE: usize = 1 << 16;
        let dir = empty_dir("reader");
        let file = dir.join("doc.bin");
        write(&file, &[b'a'; SIZE]).unwrap();

        let done = std::sync::atomic::AtomicBool::new(false);
        std::thread::scope(|scope| {
            scope.spawn(|| {
                for round in 0..60 {
                    let fill = if round % 2 == 0 { b'b' } else { b'a' };
                    write(&file, &[fill; SIZE]).unwrap();
                }
                done.store(true, Ordering::SeqCst);
            });
            while !done.load(Ordering::SeqCst) {
                // A read may lose the race for the file on Windows; what
                // matters is that a read that succeeds is whole.
                if let Ok(bytes) = fs::read(&file) {
                    assert_eq!(bytes.len(), SIZE);
                    assert!(bytes.iter().all(|byte| *byte == bytes[0]));
                }
            }
        });
        assert_eq!(names_in(&dir), ["doc.bin"]);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn concurrent_saves_of_one_file_do_not_collide() {
        let dir = empty_dir("concurrent");
        let file = dir.join("doc.bin");
        std::thread::scope(|scope| {
            for writer in 0..4u8 {
                let file = &file;
                scope.spawn(move || {
                    for _ in 0..10 {
                        write(file, &[writer; 256]).unwrap();
                    }
                });
            }
        });
        let bytes = fs::read(&file).unwrap();
        assert_eq!(bytes.len(), 256);
        assert!(bytes.iter().all(|byte| *byte == bytes[0]));
        assert_eq!(names_in(&dir), ["doc.bin"]);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_symbolic_link_keeps_pointing_at_the_replaced_file() {
        let dir = empty_dir("link");
        let real = dir.join("real.bin");
        let link = dir.join("link.bin");
        fs::write(&real, b"old").unwrap();
        #[cfg(unix)]
        let linked = std::os::unix::fs::symlink(&real, &link);
        #[cfg(windows)]
        let linked = std::os::windows::fs::symlink_file(&real, &link);
        if linked.is_err() {
            // Creating links takes a privilege Windows accounts may lack.
            let _ = fs::remove_dir_all(&dir);
            return;
        }

        write(&link, b"new").unwrap();
        assert_eq!(fs::read(&real).unwrap(), b"new");
        assert!(fs::symlink_metadata(&link).unwrap().file_type().is_symlink());
        assert_eq!(names_in(&dir), ["link.bin", "real.bin"]);
        let _ = fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn the_replaced_file_keeps_its_permissions() {
        use std::os::unix::fs::PermissionsExt as _;

        let dir = empty_dir("mode");
        let file = dir.join("doc.bin");
        fs::write(&file, b"old").unwrap();
        fs::set_permissions(&file, Permissions::from_mode(0o640)).unwrap();
        write(&file, b"new").unwrap();
        assert_eq!(fs::metadata(&file).unwrap().permissions().mode() & 0o777, 0o640);
        let _ = fs::remove_dir_all(&dir);
    }
}
