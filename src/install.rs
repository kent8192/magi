//! Binary and skill installer for the `magi` CLI.
//!
//! This module handles placing the `magi` binary into the two conventional
//! locations that the rest of the magi ecosystem expects:
//!
//! - `~/.agents/skills/magi/bin/magi` — the canonical "skill" path used by
//!   agent frameworks that look for executables under `~/.agents/skills/`.
//! - `~/.local/bin/magi` — the standard XDG-compatible user-local `PATH`
//!   location, so users can invoke `magi` directly in their shell.
//!
//! The `run` function is the entry point called by the `magi install`
//! subcommand.  It also ensures the application configuration directory
//! (`~/.magi`) is initialised with a baseline `AppConfig` before the
//! binary is copied, so the tool is immediately usable after installation.
//!
//! The copy strategy uses a write-to-temp-file followed by an atomic
//! `std::fs::rename` so a partially-written binary is never visible at the
//! target path, even if the process is interrupted mid-copy.

use std::fs;
use std::path::{Path, PathBuf};

use crate::config::{AppConfig, ConfigPaths};
use crate::error::Result;

/// The two filesystem paths where the `magi` binary is installed.
///
/// Both paths are derived from the user's home directory at construction
/// time via `InstallPaths::from_home`.
#[derive(Debug, Clone, Eq, PartialEq)]
pub struct InstallPaths {
    /// Canonical skill path: `~/.agents/skills/magi/bin/magi`.
    ///
    /// Agent frameworks that discover tools under `~/.agents/skills/` will
    /// find the binary here.
    pub skill_bin: PathBuf,

    /// XDG user-local CLI path: `~/.local/bin/magi`.
    ///
    /// This directory is typically on the user's `PATH`, making `magi`
    /// directly invocable from any shell.
    pub local_cli: PathBuf,
}

impl InstallPaths {
    /// Constructs `InstallPaths` relative to the given `home` directory.
    ///
    /// Both destination paths are derived from `home`:
    ///
    /// - `skill_bin` → `<home>/.agents/skills/magi/bin/magi`
    /// - `local_cli` → `<home>/.local/bin/magi`
    pub fn from_home(home: impl AsRef<Path>) -> Self {
        let home = home.as_ref();
        Self {
            skill_bin: home.join(".agents/skills/magi/bin/magi"),
            local_cli: home.join(".local/bin/magi"),
        }
    }
}

/// Runs the `magi install` command.
///
/// This function:
///
/// 1. Resolves configuration paths via `ConfigPaths::from_env` (requires
///    `HOME` to be set in the environment).
/// 2. Loads — or creates with defaults — the `AppConfig` and persists it,
///    so the state directory `~/.magi` exists before the binary is placed.
/// 3. Copies the currently running executable to both install locations using
///    `install_binary_from_path`.
/// 4. Prints the resulting paths to stdout so the caller can confirm success.
///
/// # Errors
///
/// Returns an error if:
/// - Required environment variables are absent or malformed.
/// - Configuration cannot be loaded or saved.
/// - The current executable path cannot be determined.
/// - Any filesystem operation (directory creation, copy, rename) fails.
///
/// # Panics
///
/// Panics if `HOME` is not set in the environment, although in practice
/// `ConfigPaths::from_env` already validates this and returns an error
/// before this path is reached.
pub async fn run() -> Result<()> {
    let paths = ConfigPaths::from_env()?;
    // Ensure the magi config/state directory exists and has a valid config.
    AppConfig::load_from_paths(&paths)?.save_to_paths(&paths)?;

    // ConfigPaths::from_env already verified HOME is present, so the
    // expect below documents that invariant rather than guarding a real risk.
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .expect("ConfigPaths::from_env already validated HOME");
    let current_exe = std::env::current_exe()?;
    let installed = install_binary_from_path(home, current_exe)?;

    println!("installed {}", installed.skill_bin.display());
    println!("installed {}", installed.local_cli.display());
    println!("config {}", paths.root.display());
    Ok(())
}

/// Copies the binary at `source` into both install locations under `home`.
///
/// Parent directories for each destination are created automatically if they
/// do not already exist.  The copy is performed atomically (write to a
/// process-unique temp file, then `std::fs::rename`) so the target is
/// never partially written.
///
/// Returns the resolved `InstallPaths` on success so the caller can report
/// or inspect the final destination paths.
///
/// # Errors
///
/// Returns an error if directory creation, the file copy, the permission
/// change, or the rename fails for either destination.
pub fn install_binary_from_path(
    home: impl AsRef<Path>,
    source: impl AsRef<Path>,
) -> Result<InstallPaths> {
    let paths = InstallPaths::from_home(home);
    install_one(&paths.skill_bin, source.as_ref())?;
    install_one(&paths.local_cli, source.as_ref())?;
    Ok(paths)
}

/// Installs a single binary from `source` to `target` atomically.
///
/// The steps are:
/// 1. Create all missing parent directories of `target`.
/// 2. Determine a process-unique temporary path alongside `target` (same
///    directory, extension `tmp.<pid>`), removing any stale copy first.
/// 3. Copy `source` to the temp path.
/// 4. Set executable permissions on the temp file (Unix: mode `0o755`).
/// 5. Atomically rename the temp file to `target`.
///
/// Using a rename for the final step ensures the target is either fully
/// replaced or untouched — a partially-copied file is never exposed at the
/// destination path.
///
/// # Errors
///
/// Propagates any I/O error from directory creation, stale-temp removal,
/// file copy, permission setting, or rename.
fn install_one(target: &Path, source: &Path) -> Result<()> {
    if let Some(parent) = target.parent() {
        // Create the full directory tree; no-ops if it already exists.
        fs::create_dir_all(parent)?;
    }

    // Build a temp path in the same directory so the final rename is
    // guaranteed to be on the same filesystem (required for atomicity on
    // most platforms).  The process ID makes the name unique, preventing
    // conflicts between concurrent install invocations.
    let tmp = target.with_extension(format!("tmp.{}", std::process::id()));
    if tmp.exists() {
        // Clean up a stale temp file left by a previous crashed install.
        fs::remove_file(&tmp)?;
    }
    // nosemgrep: rust.actix.path-traversal.tainted-path.tainted-path - install copies the current executable or test fixture into fixed user-local targets.
    fs::copy(source, &tmp)?;
    set_executable_permissions(&tmp)?;
    // Atomic rename: replaces target in a single syscall on POSIX systems.
    fs::rename(&tmp, target)?;
    Ok(())
}

/// Sets the file at `path` to be executable by owner, group, and others
/// (mode `0o755`) on Unix platforms.
///
/// # Errors
///
/// Returns an error if the underlying `std::fs::set_permissions` call
/// fails (e.g., permission denied, path does not exist).
#[cfg(unix)]
fn set_executable_permissions(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;

    fs::set_permissions(path, fs::Permissions::from_mode(0o755))?;
    Ok(())
}

/// No-op implementation of `set_executable_permissions` for non-Unix targets.
///
/// On platforms such as Windows the concept of a POSIX execute bit does not
/// apply, so this function succeeds immediately without making any changes.
#[cfg(not(unix))]
fn set_executable_permissions(_path: &Path) -> Result<()> {
    Ok(())
}
