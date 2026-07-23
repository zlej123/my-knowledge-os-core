use std::{
    collections::HashMap,
    fs,
    path::{Component, Path, PathBuf},
};

use cap_std::{
    ambient_authority,
    fs::{Dir, File},
};
use unicode_normalization::UnicodeNormalization;

use crate::error::MkoError;

pub const MAX_PORTABLE_COMPONENT_BYTES: usize = 255;
pub const MAX_PORTABLE_RELATIVE_PATH_BYTES: usize = 240;

pub struct ProviderPath {
    pub file: File,
    pub logical_path: String,
}

pub fn canonical_directory(path: &Path, error_code: &str) -> Result<PathBuf, MkoError> {
    let canonical = fs::canonicalize(path).map_err(|error| {
        MkoError::new(
            error_code,
            format!("cannot canonicalize {}: {error}", path.display()),
        )
    })?;
    if !fs::metadata(&canonical)
        .map_err(|error| MkoError::new(error_code, error.to_string()))?
        .is_dir()
    {
        return Err(MkoError::new(error_code, "path must be a directory"));
    }
    Ok(canonical)
}

pub fn provider_path(provider_root: &Path, file: &Path) -> Result<ProviderPath, MkoError> {
    provider_path_with_before_open(provider_root, file, || {})
}

fn provider_path_with_before_open<F>(
    provider_root: &Path,
    file: &Path,
    before_open: F,
) -> Result<ProviderPath, MkoError>
where
    F: FnOnce(),
{
    let absolute_file = absolute_path(file)?;
    let resolved_file = fs::canonicalize(&absolute_file).map_err(|error| {
        MkoError::new(
            "file_unreadable",
            format!("cannot resolve {}: {error}", absolute_file.display()),
        )
    })?;
    let relative = resolved_file.strip_prefix(provider_root).map_err(|_| {
        MkoError::new(
            "outside_allowed_root",
            "file is outside the configured provider root",
        )
    })?;
    let provider = Dir::open_ambient_dir(provider_root, ambient_authority()).map_err(|error| {
        MkoError::new(
            "file_unreadable",
            format!(
                "cannot open provider root {}: {error}",
                provider_root.display()
            ),
        )
    })?;
    let canonical_relative = provider.canonicalize(relative).map_err(|_| {
        MkoError::new(
            "outside_allowed_root",
            "file is outside the configured provider root",
        )
    })?;
    let logical_path = normalized_logical_path(&canonical_relative)?;
    reject_capability_collisions(&provider, &canonical_relative)?;
    before_open();
    let file = provider.open(&canonical_relative).map_err(|_| {
        MkoError::new(
            "outside_allowed_root",
            "file could not be opened safely within the configured provider root",
        )
    })?;
    if !file
        .metadata()
        .map_err(|error| MkoError::new("file_unreadable", error.to_string()))?
        .is_file()
    {
        return Err(MkoError::new(
            "file_unreadable",
            "capture input must be a regular file",
        ));
    }

    Ok(ProviderPath { file, logical_path })
}

pub fn registry_directory(repository_root: &Path) -> Result<PathBuf, MkoError> {
    let directory = repository_root.join("assets").join("registry");
    fs::create_dir_all(&directory).map_err(|error| {
        MkoError::new(
            "registry_write_failed",
            format!("cannot create {}: {error}", directory.display()),
        )
    })?;
    let canonical = fs::canonicalize(&directory).map_err(|error| {
        MkoError::new(
            "registry_write_failed",
            format!("cannot canonicalize {}: {error}", directory.display()),
        )
    })?;
    if !canonical.starts_with(repository_root) {
        return Err(MkoError::new(
            "outside_allowed_root",
            "registry directory escapes the repository root",
        ));
    }
    reject_directory_collisions(&canonical)?;
    Ok(canonical)
}

pub fn validate_ascii_slug(slug: &str) -> Result<(), MkoError> {
    if slug.is_empty()
        || !slug
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
    {
        return Err(MkoError::new(
            "invalid_slug",
            "slug must contain only lowercase ASCII letters, digits, and hyphens",
        ));
    }
    reject_windows_component(slug)?;
    Ok(())
}

pub fn validate_portable_relative_path(path: &str) -> Result<(), MkoError> {
    if path.is_empty()
        || path.len() > MAX_PORTABLE_RELATIVE_PATH_BYTES
        || path.starts_with('/')
        || path.starts_with('\\')
        || path.contains('\\')
    {
        return Err(MkoError::new(
            "path_not_portable",
            "path must be a bounded portable relative path",
        ));
    }
    for component in path.split('/') {
        if component.is_empty()
            || component == "."
            || component == ".."
            || component.len() > MAX_PORTABLE_COMPONENT_BYTES
        {
            return Err(MkoError::new(
                "path_not_portable",
                "path contains an empty, traversal, or oversized component",
            ));
        }
        reject_windows_component(component).map_err(|_| {
            MkoError::new(
                "path_not_portable",
                "path contains a Windows-incompatible component",
            )
        })?;
    }
    Ok(())
}

fn normalized_logical_path(path: &Path) -> Result<String, MkoError> {
    let mut components = Vec::new();
    for component in path.components() {
        let Component::Normal(value) = component else {
            return Err(MkoError::new(
                "invalid_path",
                "logical paths must not contain traversal",
            ));
        };
        let value = value
            .to_str()
            .ok_or_else(|| MkoError::new("invalid_path", "logical paths must be valid UTF-8"))?;
        let normalized = value.nfc().collect::<String>();
        if normalized.is_empty() {
            return Err(MkoError::new(
                "invalid_path",
                "logical path component is empty",
            ));
        }
        reject_windows_component(&normalized)?;
        components.push(normalized);
    }
    if components.is_empty() {
        return Err(MkoError::new(
            "invalid_path",
            "logical path must name a file",
        ));
    }
    Ok(components.join("/"))
}

fn reject_capability_collisions(root: &Dir, relative: &Path) -> Result<(), MkoError> {
    let mut current = root
        .open_dir(".")
        .map_err(|error| MkoError::new("file_unreadable", error.to_string()))?;
    let components = relative.components().collect::<Vec<_>>();
    for (index, component) in components.iter().enumerate() {
        let Component::Normal(name) = component else {
            return Err(MkoError::new(
                "invalid_path",
                "logical paths must not contain traversal",
            ));
        };
        reject_component_collision(&current, name)?;
        if index + 1 != components.len() {
            current = current.open_dir(name).map_err(|_| {
                MkoError::new(
                    "outside_allowed_root",
                    "provider path escapes the configured provider root",
                )
            })?;
        }
    }
    Ok(())
}

fn reject_directory_collisions(directory: &Path) -> Result<(), MkoError> {
    let mut names = HashMap::new();
    for entry in fs::read_dir(directory)
        .map_err(|error| MkoError::new("registry_write_failed", error.to_string()))?
    {
        let entry =
            entry.map_err(|error| MkoError::new("registry_write_failed", error.to_string()))?;
        let name = entry.file_name();
        let name = name.to_str().ok_or_else(|| {
            MkoError::new("invalid_path", "repository filename must be valid UTF-8")
        })?;
        let key = collision_key(name);
        if names.insert(key, name.to_owned()).is_some() {
            return Err(MkoError::new(
                "path_collision",
                "repository contains a case or Unicode-normalization filename collision",
            ));
        }
    }
    Ok(())
}

fn reject_component_collision(directory: &Dir, expected: &std::ffi::OsStr) -> Result<(), MkoError> {
    let expected = expected
        .to_str()
        .ok_or_else(|| MkoError::new("invalid_path", "provider filename must be valid UTF-8"))?;
    let expected_key = collision_key(expected);
    let mut matches = Vec::new();
    for entry in directory
        .entries()
        .map_err(|error| MkoError::new("file_unreadable", error.to_string()))?
    {
        let entry = entry.map_err(|error| MkoError::new("file_unreadable", error.to_string()))?;
        let name = entry.file_name();
        let name = name.to_str().ok_or_else(|| {
            MkoError::new("invalid_path", "provider filename must be valid UTF-8")
        })?;
        if collision_key(name) == expected_key {
            matches.push(name.to_owned());
        }
    }
    if matches.len() != 1 || matches[0] != expected {
        return Err(MkoError::new(
            "path_collision",
            "provider contains a case or Unicode-normalization filename collision",
        ));
    }
    Ok(())
}

fn absolute_path(path: &Path) -> Result<PathBuf, MkoError> {
    if path.is_absolute() {
        return Ok(path.to_path_buf());
    }
    std::env::current_dir()
        .map(|directory| directory.join(path))
        .map_err(|error| MkoError::new("file_unreadable", error.to_string()))
}

fn collision_key(value: &str) -> String {
    value.nfc().collect::<String>().to_lowercase()
}

fn reject_windows_component(value: &str) -> Result<(), MkoError> {
    if value.len() > MAX_PORTABLE_COMPONENT_BYTES
        || value.chars().any(|character| {
            character <= '\u{1f}'
                || matches!(character, '<' | '>' | ':' | '"' | '\\' | '|' | '?' | '*')
        })
    {
        return Err(MkoError::new(
            "windows_reserved_name",
            "path contains a Windows-forbidden character or oversized component",
        ));
    }
    let trimmed = value.trim_end_matches(['.', ' ']);
    if trimmed != value || trimmed.is_empty() {
        return Err(MkoError::new(
            "windows_reserved_name",
            "path components may not end in a dot or space",
        ));
    }
    let stem = trimmed
        .split('.')
        .next()
        .unwrap_or_default()
        .to_ascii_uppercase();
    let reserved = matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || (stem.len() == 4
            && (stem.starts_with("COM") || stem.starts_with("LPT"))
            && matches!(stem.as_bytes()[3], b'1'..=b'9'));
    if reserved {
        return Err(MkoError::new(
            "windows_reserved_name",
            "path contains a Windows reserved name",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #[cfg(unix)]
    use std::{
        fs,
        sync::atomic::{AtomicU64, Ordering},
    };

    #[cfg(unix)]
    use super::provider_path_with_before_open;
    use super::{
        MAX_PORTABLE_COMPONENT_BYTES, MAX_PORTABLE_RELATIVE_PATH_BYTES,
        validate_portable_relative_path,
    };

    #[cfg(unix)]
    static NEXT_TEST_ENV: AtomicU64 = AtomicU64::new(0);

    #[cfg(unix)]
    #[test]
    fn swap_to_outside_symlink_cannot_redirect_capability_read() {
        let unique = NEXT_TEST_ENV.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!(
            "mko-provider-swap-test-{}-{unique}",
            std::process::id()
        ));
        let provider = root.join("provider");
        let outside = root.join("outside.pdf");
        let candidate = provider.join("paper.pdf");
        fs::create_dir_all(&provider).unwrap();
        fs::write(&candidate, b"%PDF-1.7\nsafe").unwrap();
        fs::write(&outside, b"%PDF-1.7\noutside").unwrap();

        let error = provider_path_with_before_open(&provider, &candidate, || {
            fs::remove_file(&candidate).unwrap();
            std::os::unix::fs::symlink(&outside, &candidate).unwrap();
        })
        .err()
        .unwrap();

        assert_eq!(error.code(), "outside_allowed_root");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn portable_paths_reject_cross_platform_hazards_and_bounds() {
        for path in [
            "../secret",
            "/absolute",
            "C:/windows",
            "notes\\windows.md",
            "notes/CON.txt",
            "notes/trailing. ",
            "notes/forbidden?.md",
            "notes/control\u{001f}.md",
        ] {
            assert!(validate_portable_relative_path(path).is_err(), "{path}");
        }
        assert!(
            validate_portable_relative_path(&format!(
                "notes/{}",
                "a".repeat(MAX_PORTABLE_COMPONENT_BYTES + 1)
            ))
            .is_err()
        );
        assert!(
            validate_portable_relative_path(&format!(
                "{}/file.md",
                "a".repeat(MAX_PORTABLE_RELATIVE_PATH_BYTES)
            ))
            .is_err()
        );
        assert!(validate_portable_relative_path("notes/good-file.md").is_ok());
    }
}
