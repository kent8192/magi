//! Helpers for safely validating and identifying OS processes referenced by
//! pid files. Shared by the Redis and SSH lifecycle managers so both apply the
//! same checks before signalling a recorded pid.
//!
//! The module exposes three tiers of process-safety utilities:
//!
//! 1. **Pid-file parsing** (`parse_pid_file_contents`) — converts raw file
//!    text into a validated, non-zero `u32` so a corrupt or adversarial pid
//!    file can never reach the kernel's `kill` syscall with a dangerous value
//!    (e.g. `-1` or `0`).
//! 2. **Process identity** (`executable_name`, `executable_basename_is`) —
//!    confirms that the process currently occupying a recorded pid is actually
//!    the program we started, preventing accidental signalling of a recycled
//!    pid after the original process has exited.
//! 3. **Atomic pid-file cleanup** (`remove_file_if_exists`) — removes a stale
//!    pid file after a process exits, treating a pre-existing absence as
//!    success so callers do not need to guard with an existence check.

use std::path::Path;
use std::process::Stdio;

use tokio::process::Command;

use crate::error::{MagiError, Result};

/// Parse the contents of a pid file into a positive process id.
///
/// Returns `Ok(None)` for an empty file (nothing to act on) and an error for
/// any non-numeric or non-positive value, so a malformed pid file can never be
/// forwarded verbatim to `kill` (where values such as `-1` would target every
/// process the user may signal).
///
/// # Errors
///
/// Returns `MagiError::InvalidConfig` when the file contents are non-empty
/// but cannot be parsed as a positive `u32` (e.g. a negative number, zero, or
/// non-numeric text).
pub fn parse_pid_file_contents(contents: &str) -> Result<Option<u32>> {
    let trimmed = contents.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }

    // `parse::<u32>()` already rejects negatives and non-numeric text; the
    // explicit zero check below guards against a file containing "0", which
    // parses successfully as a `u32` but is not a valid process identifier.
    let pid = trimmed.parse::<u32>().map_err(|_| {
        MagiError::InvalidConfig("pid file must contain a positive integer".to_string())
    })?;
    if pid == 0 {
        return Err(MagiError::InvalidConfig(
            "pid file must contain a positive integer".to_string(),
        ));
    }

    Ok(Some(pid))
}

/// Resolve the executable name backing a pid via `ps`. Returns `Ok(None)` when
/// the process is not running.
///
/// The `ps` invocation uses the `-o comm=` format specifier, which omits the
/// column header and yields only the command name (a bare basename on Linux;
/// an absolute path on macOS). Both `executable_basename_is` and the callers
/// normalise this difference via `Path::file_name`.
///
/// `stdin` and `stderr` are suppressed so that `ps` output never leaks into
/// the user's terminal or the magi log stream.
///
/// # Errors
///
/// Returns an error if spawning the `ps` subprocess fails (e.g. the binary is
/// not on `PATH` or the OS denies the fork).
pub async fn executable_name(pid: u32) -> Result<Option<String>> {
    let output = Command::new("ps")
        .args(["-p", &pid.to_string(), "-o", "comm="])
        .stdin(Stdio::null())
        // Suppress ps error output (e.g. "no process found") so it does not
        // reach the user's terminal; a non-zero exit status is handled below.
        .stderr(Stdio::null())
        .output()
        .await?;

    // `ps` exits non-zero when no process with the given pid exists.
    if !output.status.success() {
        return Ok(None);
    }

    let comm = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if comm.is_empty() {
        Ok(None)
    } else {
        Ok(Some(comm))
    }
}

/// Whether an executable name (a bare name on Linux or a full path on macOS)
/// has `program` as its file-name component. Uses an exact match so a recorded
/// pid that has been reused by an unrelated program is not mistaken for ours.
///
/// # Parameters
///
/// - `name` — the value returned by `executable_name`, which may be a bare
///   basename (`redis-server`) or an absolute path
///   (`/opt/homebrew/bin/redis-server`) depending on the host OS.
/// - `program` — the expected bare filename to match against (e.g.
///   `"redis-server"`).
pub fn executable_basename_is(name: &str, program: &str) -> bool {
    Path::new(name)
        // `file_name` strips directory components so both the Linux bare-name
        // and macOS full-path forms compare against the same basename.
        .file_name()
        .and_then(|base| base.to_str())
        .is_some_and(|base| base == program)
}

/// Remove a file if it exists, treating a missing file as success.
///
/// This is the idiomatic cleanup pattern for pid files: the caller does not
/// need to check for existence first, and a race where another process removed
/// the file between an existence check and a removal call is handled safely.
///
/// # Errors
///
/// Returns an error for any `std::io::Error` other than
/// `std::io::ErrorKind::NotFound`, such as a permissions failure.
pub fn remove_file_if_exists(path: &Path) -> Result<()> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        // NotFound is not an error — the file is already absent, which is the
        // desired end state.
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}
