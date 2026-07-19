use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
};

use crate::{atomic::write_new, error::MkoError};

pub const PRE_COMMIT_SCRIPT: &str = "#!/usr/bin/env bash\n# My Knowledge OS pre-commit v0.1\nset -euo pipefail\nmko check --repo \"$(git rev-parse --show-toplevel)\" --staged\n";

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HookInstallResult {
    pub result: String,
    pub hook_path: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HookState {
    Missing,
    Managed,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HookInspection {
    pub repository_root: PathBuf,
    pub state: HookState,
}

pub fn inspect_hook(repository_root: &Path) -> Result<HookInspection, MkoError> {
    let repository_root = canonical_repository_root(repository_root)?;
    validate_git_root(&repository_root)?;
    let configured_path = configured_hook_path(&repository_root)?;
    if configured_path
        .as_deref()
        .is_some_and(|path| path != ".githooks")
    {
        return Err(MkoError::new(
            "hook_conflict",
            "custom core.hooksPath is configured; preserve it explicitly",
        ));
    }

    let directory = repository_root.join(".githooks");
    match fs::symlink_metadata(&directory) {
        Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {}
        Ok(_) => {
            return Err(MkoError::new(
                "hook_path_invalid",
                ".githooks must be a real directory",
            ));
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(HookInspection {
                repository_root,
                state: HookState::Missing,
            });
        }
        Err(error) => return Err(MkoError::new("hook_inspection_failed", error.to_string())),
    }

    let destination = directory.join("pre-commit");
    let hook_is_managed = match fs::symlink_metadata(&destination) {
        Ok(metadata) if metadata.is_file() && !metadata.file_type().is_symlink() => {
            let existing = fs::read(&destination)
                .map_err(|error| MkoError::new("hook_inspection_failed", error.to_string()))?;
            if existing != PRE_COMMIT_SCRIPT.as_bytes() {
                return Err(MkoError::new(
                    "hook_conflict",
                    "existing pre-commit hook is materially different; preserve or integrate it explicitly",
                ));
            }
            true
        }
        Ok(_) => {
            return Err(MkoError::new(
                "hook_path_invalid",
                "pre-commit hook must be a regular file",
            ));
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
        Err(error) => return Err(MkoError::new("hook_inspection_failed", error.to_string())),
    };

    Ok(HookInspection {
        repository_root,
        state: if hook_is_managed && configured_path.as_deref() == Some(".githooks") {
            HookState::Managed
        } else {
            HookState::Missing
        },
    })
}

pub fn install_hooks(repository_root: &Path) -> Result<HookInstallResult, MkoError> {
    let inspection = inspect_hook(repository_root)?;
    let repository_root = inspection.repository_root;
    if inspection.state == HookState::Managed {
        return Ok(HookInstallResult {
            result: "installed".into(),
            hook_path: ".githooks/pre-commit".into(),
        });
    }
    let directory = repository_root.join(".githooks");
    match fs::symlink_metadata(&directory) {
        Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {}
        Ok(_) => {
            return Err(MkoError::new(
                "hook_path_invalid",
                ".githooks must be a real directory",
            ));
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => fs::create_dir(&directory)
            .map_err(|error| MkoError::new("hook_install_failed", error.to_string()))?,
        Err(error) => return Err(MkoError::new("hook_install_failed", error.to_string())),
    }
    let destination = directory.join("pre-commit");
    match fs::symlink_metadata(&destination) {
        Ok(metadata) if !metadata.is_file() || metadata.file_type().is_symlink() => {
            return Err(MkoError::new(
                "hook_path_invalid",
                "pre-commit hook must be a regular file",
            ));
        }
        Ok(_) => {
            let existing = fs::read(&destination)
                .map_err(|error| MkoError::new("hook_install_failed", error.to_string()))?;
            if existing != PRE_COMMIT_SCRIPT.as_bytes() {
                return Err(MkoError::new(
                    "hook_conflict",
                    "existing pre-commit hook is materially different; preserve or integrate it explicitly",
                ));
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            write_new(&destination, PRE_COMMIT_SCRIPT.as_bytes(), |_| Ok(()))?;
        }
        Err(error) => return Err(MkoError::new("hook_install_failed", error.to_string())),
    }
    make_executable(&destination)?;
    let status = Command::new("git")
        .arg("-C")
        .arg(&repository_root)
        .args(["config", "--local", "core.hooksPath", ".githooks"])
        .status()
        .map_err(|error| MkoError::new("hook_install_failed", error.to_string()))?;
    if !status.success() {
        return Err(MkoError::new("hook_install_failed", "git config failed"));
    }
    Ok(HookInstallResult {
        result: "installed".into(),
        hook_path: ".githooks/pre-commit".into(),
    })
}

fn canonical_repository_root(repository_root: &Path) -> Result<PathBuf, MkoError> {
    fs::canonicalize(repository_root).map_err(|error| {
        MkoError::new(
            "repository_root_invalid",
            format!("cannot resolve repository: {error}"),
        )
    })
}

fn configured_hook_path(repository_root: &Path) -> Result<Option<String>, MkoError> {
    let output = Command::new("git")
        .arg("-C")
        .arg(repository_root)
        .args(["config", "--local", "--get", "core.hooksPath"])
        .output()
        .map_err(|error| MkoError::new("git_unavailable", error.to_string()))?;
    if output.status.success() {
        let value = String::from_utf8(output.stdout)
            .map_err(|_| MkoError::new("hook_inspection_failed", "hook path is not UTF-8"))?;
        return Ok(Some(value.trim().into()));
    }
    if output.status.code() == Some(1) {
        return Ok(None);
    }
    Err(MkoError::new(
        "hook_inspection_failed",
        "cannot inspect local core.hooksPath",
    ))
}

fn validate_git_root(repository_root: &Path) -> Result<(), MkoError> {
    let output = Command::new("git")
        .arg("-C")
        .arg(repository_root)
        .args(["rev-parse", "--show-toplevel"])
        .output()
        .map_err(|error| MkoError::new("git_unavailable", error.to_string()))?;
    if !output.status.success() {
        return Err(MkoError::new(
            "git_repository_required",
            "repository is not a Git worktree",
        ));
    }
    let top = String::from_utf8(output.stdout)
        .map_err(|_| MkoError::new("git_repository_required", "Git root is not UTF-8"))?;
    let top = fs::canonicalize(PathBuf::from(top.trim()))
        .map_err(|error| MkoError::new("git_repository_required", error.to_string()))?;
    if top != repository_root {
        return Err(MkoError::new(
            "git_repository_required",
            "--repo must name the Git worktree root",
        ));
    }
    Ok(())
}

#[cfg(unix)]
fn make_executable(path: &Path) -> Result<(), MkoError> {
    use std::os::unix::fs::PermissionsExt;
    let mut permissions = fs::metadata(path)
        .map_err(|error| MkoError::new("hook_install_failed", error.to_string()))?
        .permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(path, permissions)
        .map_err(|error| MkoError::new("hook_install_failed", error.to_string()))
}

#[cfg(not(unix))]
fn make_executable(_path: &Path) -> Result<(), MkoError> {
    Ok(())
}
