//! Installation of the `gitrx` command-line launcher from inside the app.
//!
//! Mirrors `scripts/install-cli.sh`: the launcher is written to the first
//! writable of `/opt/homebrew/bin` or `/usr/local/bin`; if neither is writable
//! it escalates once via a native admin prompt (`osascript ... with
//! administrator privileges`) targeting `/usr/local/bin`.
//!
//! The launcher source is embedded at compile time (`include_str!`) so the
//! installed command never depends on the project directory staying put.

use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use tauri::{AppHandle, Runtime};
use tauri_plugin_dialog::{DialogExt, MessageDialogKind};

/// The `gitrx` launcher, embedded at build time. Path is relative to this
/// source file: `src-tauri/src/` -> repo root -> `scripts/gitrx`.
const GITRX_SCRIPT: &str = include_str!("../../scripts/gitrx");

/// Installed command name and the directories searched, in priority order.
const CMD: &str = "gitrx";
const CANDIDATE_DIRS: &[&str] = &["/opt/homebrew/bin", "/usr/local/bin"];
/// Directory used for the elevated fallback (matches install-cli.sh).
const ELEVATED_DIR: &str = "/usr/local/bin";

/// Outcome of an install attempt. `Cancelled` (the user dismissed the admin
/// prompt) is a clean, silent result — not an error.
enum InstallOutcome {
    Installed(PathBuf),
    Cancelled,
}

/// Entry point wired to the "Install Command Line Tool…" menu item.
///
/// Intended to run off the main thread; it may block on an elevation prompt and
/// on the modal result dialog. All UI is surfaced through `AppHandle` dialogs.
pub fn install_cli<R: Runtime>(app: AppHandle<R>) {
    let candidates: Vec<PathBuf> = CANDIDATE_DIRS.iter().map(PathBuf::from).collect();

    let result = match choose_install_dir(&candidates) {
        Some(dir) => install_directly(&dir),
        None => install_elevated(),
    };

    match result {
        Ok(InstallOutcome::Installed(path)) => {
            app.dialog()
                .message(format!(
                    "Installed {CMD} to {}.\n\nTry: {CMD} .",
                    path.display()
                ))
                .title("Command Line Tool")
                .kind(MessageDialogKind::Info)
                .blocking_show();
        }
        Ok(InstallOutcome::Cancelled) => {
            // User dismissed the authorization prompt. Nothing to report.
        }
        Err(reason) => {
            app.dialog()
                .message(format!("Could not install {CMD}.\n\n{reason}"))
                .title("Install Failed")
                .kind(MessageDialogKind::Error)
                .blocking_show();
        }
    }
}

/// Pick the first candidate that already exists as a writable directory.
///
/// Pure and side-effect-free aside from a create/delete write probe (the only
/// portable way to reflect `-w` semantics for root-owned dirs). Returns `None`
/// when no candidate is a writable directory, signaling the elevated fallback.
pub fn choose_install_dir(candidates: &[PathBuf]) -> Option<PathBuf> {
    candidates.iter().find(|dir| is_writable_dir(dir)).cloned()
}

/// True when `dir` exists, is a directory, and a file can be created in it.
fn is_writable_dir(dir: &Path) -> bool {
    if !dir.is_dir() {
        return false;
    }
    let probe = dir.join(format!(".{CMD}-write-probe-{}", unique_suffix()));
    match std::fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(0o600)
        .open(&probe)
    {
        Ok(_) => {
            let _ = std::fs::remove_file(&probe);
            true
        }
        Err(_) => false,
    }
}

/// Write the embedded launcher directly into `dir` with mode 0755 (an update if
/// one already exists).
fn install_directly(dir: &Path) -> Result<InstallOutcome, String> {
    let dest = dir.join(CMD);
    write_script_0755(&dest).map_err(|e| format!("Writing {}: {e}", dest.display()))?;
    Ok(InstallOutcome::Installed(dest))
}

/// Escalate: stage the script in a private temp file, then run a single
/// elevated `install(1)` via the native admin prompt. The script contents are
/// never interpolated into the AppleScript — only the quoted temp path is.
fn install_elevated() -> Result<InstallOutcome, String> {
    let tmp = write_temp_script().map_err(|e| format!("Preparing installer: {e}"))?;
    let dest = format!("{ELEVATED_DIR}/{CMD}");

    let shell_cmd = format!(
        "/bin/mkdir -p {dir} && /usr/bin/install -m 0755 {src} {dst}",
        dir = shell_single_quote(ELEVATED_DIR),
        src = shell_single_quote(&tmp.to_string_lossy()),
        dst = shell_single_quote(&dest),
    );
    let applescript = format!(
        "do shell script \"{}\" with administrator privileges",
        applescript_escape(&shell_cmd)
    );

    let output = std::process::Command::new("osascript")
        .arg("-e")
        .arg(&applescript)
        .output();

    // Best-effort cleanup of the staged copy regardless of outcome.
    let _ = std::fs::remove_file(&tmp);

    let output = output.map_err(|e| format!("Launching authorization prompt: {e}"))?;
    if output.status.success() {
        return Ok(InstallOutcome::Installed(PathBuf::from(dest)));
    }

    let stderr = String::from_utf8_lossy(&output.stderr);
    if is_user_cancellation(&stderr) {
        Ok(InstallOutcome::Cancelled)
    } else {
        let detail = stderr.trim();
        let detail = if detail.is_empty() {
            "authorization failed"
        } else {
            detail
        };
        Err(format!("Elevated install failed: {detail}"))
    }
}

/// Write the embedded script to `dest` and mark it executable (0755).
fn write_script_0755(dest: &Path) -> std::io::Result<()> {
    std::fs::write(dest, GITRX_SCRIPT)?;
    std::fs::set_permissions(dest, std::fs::Permissions::from_mode(0o755))
}

/// Stage the embedded script in a fresh, private (0600) temp file created with
/// `O_EXCL`, so no pre-existing file or symlink can be clobbered.
fn write_temp_script() -> std::io::Result<PathBuf> {
    use std::io::Write;

    let path = std::env::temp_dir().join(format!(
        "{CMD}-install-{}-{}",
        std::process::id(),
        unique_suffix()
    ));
    let mut file = std::fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(0o600)
        .open(&path)?;
    file.write_all(GITRX_SCRIPT.as_bytes())?;
    Ok(path)
}

/// A short, monotonic-ish suffix for probe/temp filenames.
fn unique_suffix() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0)
}

/// Wrap `s` in single quotes for `/bin/sh`, escaping embedded single quotes.
fn shell_single_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

/// Escape a string for embedding inside an AppleScript double-quoted literal.
fn applescript_escape(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

/// Detect the "user cancelled the auth dialog" case (AppleScript error -128).
fn is_user_cancellation(stderr: &str) -> bool {
    stderr.contains("-128") || stderr.contains("User canceled")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn read_only(dir: &Path) {
        fs::set_permissions(dir, fs::Permissions::from_mode(0o555)).unwrap();
    }

    #[test]
    fn chooses_first_writable_candidate() {
        let a = tempfile::tempdir().unwrap();
        let b = tempfile::tempdir().unwrap();
        let candidates = vec![a.path().to_path_buf(), b.path().to_path_buf()];
        assert_eq!(
            choose_install_dir(&candidates),
            Some(a.path().to_path_buf())
        );
    }

    #[test]
    fn falls_through_to_second_writable_candidate() {
        let first = tempfile::tempdir().unwrap();
        let second = tempfile::tempdir().unwrap();
        read_only(first.path());
        let candidates = vec![first.path().to_path_buf(), second.path().to_path_buf()];

        let chosen = choose_install_dir(&candidates);
        // Restore perms so TempDir cleanup can remove the directory.
        fs::set_permissions(first.path(), fs::Permissions::from_mode(0o755)).unwrap();

        assert_eq!(chosen, Some(second.path().to_path_buf()));
    }

    #[test]
    fn returns_none_when_no_candidate_is_writable() {
        let ro = tempfile::tempdir().unwrap();
        read_only(ro.path());
        let missing = ro.path().join("does-not-exist");
        let candidates = vec![missing, ro.path().to_path_buf()];

        let chosen = choose_install_dir(&candidates);
        fs::set_permissions(ro.path(), fs::Permissions::from_mode(0o755)).unwrap();

        assert_eq!(chosen, None);
    }

    #[test]
    fn shell_single_quote_escapes_quotes() {
        assert_eq!(shell_single_quote("/tmp/a b"), "'/tmp/a b'");
        assert_eq!(shell_single_quote("it's"), r"'it'\''s'");
    }

    #[test]
    fn applescript_escape_escapes_backslash_and_quote() {
        assert_eq!(applescript_escape(r#"a"b\c"#), r#"a\"b\\c"#);
    }

    #[test]
    fn detects_user_cancellation() {
        assert!(is_user_cancellation(
            "execution error: User canceled. (-128)"
        ));
        assert!(!is_user_cancellation("execution error: something else (1)"));
    }
}
