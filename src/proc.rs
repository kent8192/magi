//! Helpers for safely validating and identifying OS processes referenced by
//! pid files. Shared by the Redis and SSH lifecycle managers so both apply the
//! same checks before signalling a recorded pid.

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
pub fn parse_pid_file_contents(contents: &str) -> Result<Option<u32>> {
    let trimmed = contents.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }

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
pub async fn executable_name(pid: u32) -> Result<Option<String>> {
    let output = Command::new("ps")
        .args(["-p", &pid.to_string(), "-o", "comm="])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .await?;

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
pub fn executable_basename_is(name: &str, program: &str) -> bool {
    Path::new(name)
        .file_name()
        .and_then(|base| base.to_str())
        .is_some_and(|base| base == program)
}

/// Remove a file if it exists, treating a missing file as success.
pub fn remove_file_if_exists(path: &Path) -> Result<()> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}
