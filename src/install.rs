use std::fs;
use std::path::{Path, PathBuf};

use crate::config::{AppConfig, ConfigPaths};
use crate::error::Result;

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct InstallPaths {
    pub skill_bin: PathBuf,
    pub local_cli: PathBuf,
}

impl InstallPaths {
    pub fn from_home(home: impl AsRef<Path>) -> Self {
        let home = home.as_ref();
        Self {
            skill_bin: home.join(".agents/skills/magi/bin/magi"),
            local_cli: home.join(".local/bin/magi"),
        }
    }
}

pub async fn run() -> Result<()> {
    let paths = ConfigPaths::from_env()?;
    AppConfig::load_from_paths(&paths)?.save_to_paths(&paths)?;

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

pub fn install_binary_from_path(
    home: impl AsRef<Path>,
    source: impl AsRef<Path>,
) -> Result<InstallPaths> {
    let paths = InstallPaths::from_home(home);
    install_one(&paths.skill_bin, source.as_ref())?;
    install_one(&paths.local_cli, source.as_ref())?;
    Ok(paths)
}

fn install_one(target: &Path, source: &Path) -> Result<()> {
    if let Some(parent) = target.parent() {
        fs::create_dir_all(parent)?;
    }

    let tmp = target.with_extension(format!("tmp.{}", std::process::id()));
    if tmp.exists() {
        fs::remove_file(&tmp)?;
    }
    // nosemgrep: rust.actix.path-traversal.tainted-path.tainted-path - install copies the current executable or test fixture into fixed user-local targets.
    fs::copy(source, &tmp)?;
    set_executable_permissions(&tmp)?;
    fs::rename(&tmp, target)?;
    Ok(())
}

#[cfg(unix)]
fn set_executable_permissions(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;

    fs::set_permissions(path, fs::Permissions::from_mode(0o755))?;
    Ok(())
}

#[cfg(not(unix))]
fn set_executable_permissions(_path: &Path) -> Result<()> {
    Ok(())
}
