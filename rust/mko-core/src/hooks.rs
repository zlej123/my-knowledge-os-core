use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
};

use crate::{atomic::write_new, config_v2::KnowledgeConfigV2, error::MkoError};

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
            hook_is_executable(&metadata)
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
    refuse_on_v2_knowledge_base(repository_root)?;
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

/// The pre-commit script runs `mko check`, which reads the v0.1 record
/// model — YAML front matter on every Source and Knowledge file. A v0.3
/// knowledge base stores revisions as `# Source revision` plus canonical JSON
/// and has no front matter at all, so the check rejects every record it holds.
/// Installing it therefore does not protect a v0.3 repository; it makes every
/// `git commit` fail. Found on the first real commit of the owner's live
/// knowledge base, minutes after doctor had demanded the hook.
fn refuse_on_v2_knowledge_base(repository_root: &Path) -> Result<(), MkoError> {
    let repository_root = canonical_repository_root(repository_root)?;
    if KnowledgeConfigV2::read(&repository_root).is_ok() {
        return Err(MkoError::new(
            "hook_not_supported",
            "the pre-commit check reads v0.1 records only and would reject every v0.3 revision; no hook was installed",
        ));
    }
    Ok(())
}

fn canonical_repository_root(repository_root: &Path) -> Result<PathBuf, MkoError> {
    fs::canonicalize(repository_root).map_err(|error| {
        MkoError::new(
            "repository_root_invalid",
            format!("cannot resolve repository: {error}"),
        )
    })
}

pub(crate) fn configured_hook_path(repository_root: &Path) -> Result<Option<String>, MkoError> {
    configured_hook_path_with_overrides(repository_root, None)
}

#[cfg(test)]
fn configured_hook_path_with_files(
    repository_root: &Path,
    global: &Path,
    system: &Path,
) -> Result<Option<String>, MkoError> {
    configured_hook_path_with_overrides(repository_root, Some((global, system)))
}

fn configured_hook_path_with_overrides(
    repository_root: &Path,
    isolated_files: Option<(&Path, &Path)>,
) -> Result<Option<String>, MkoError> {
    let mut command = Command::new("git");
    command
        .arg("-C")
        .arg(repository_root)
        .args(["config", "--get", "core.hooksPath"]);
    if let Some((global, system)) = isolated_files {
        command
            .env("GIT_CONFIG_GLOBAL", global)
            .env("GIT_CONFIG_SYSTEM", system)
            .env_remove("GIT_CONFIG_NOSYSTEM");
    }
    let output = command
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
        "cannot inspect effective core.hooksPath",
    ))
}

#[cfg(unix)]
fn hook_is_executable(metadata: &fs::Metadata) -> bool {
    use std::os::unix::fs::PermissionsExt;

    metadata.permissions().mode() & 0o100 != 0
}

#[cfg(not(unix))]
fn hook_is_executable(_metadata: &fs::Metadata) -> bool {
    true
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

#[cfg(test)]
mod tests {
    use std::{fs, path::PathBuf, process::Command};

    use tempfile::TempDir;

    use super::configured_hook_path_with_files;

    #[test]
    fn effective_global_hook_path_is_read_from_isolated_config() {
        let fixture = GitConfigFixture::new();
        fs::write(&fixture.global, "[core]\n\thooksPath = global-hooks\n").unwrap();

        assert_eq!(
            configured_hook_path_with_files(&fixture.repository, &fixture.global, &fixture.system,)
                .unwrap()
                .as_deref(),
            Some("global-hooks")
        );
    }

    #[test]
    fn effective_system_hook_path_is_read_from_isolated_config() {
        let fixture = GitConfigFixture::new();
        fs::write(&fixture.system, "[core]\n\thooksPath = system-hooks\n").unwrap();

        assert_eq!(
            configured_hook_path_with_files(&fixture.repository, &fixture.global, &fixture.system,)
                .unwrap()
                .as_deref(),
            Some("system-hooks")
        );
    }

    struct GitConfigFixture {
        _root: TempDir,
        repository: PathBuf,
        global: PathBuf,
        system: PathBuf,
    }

    impl GitConfigFixture {
        fn new() -> Self {
            let root = tempfile::tempdir().unwrap();
            let repository = root.path().join("repository");
            let global = root.path().join("global.gitconfig");
            let system = root.path().join("system.gitconfig");
            fs::create_dir(&repository).unwrap();
            fs::write(&global, "").unwrap();
            fs::write(&system, "").unwrap();
            let status = Command::new("git")
                .arg("-C")
                .arg(&repository)
                .args(["init", "--quiet"])
                .status()
                .unwrap();
            assert!(status.success());
            Self {
                _root: root,
                repository,
                global,
                system,
            }
        }
    }
}
