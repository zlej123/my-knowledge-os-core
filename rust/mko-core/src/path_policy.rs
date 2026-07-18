use std::{
    collections::HashMap,
    fs,
    path::{Component, Path, PathBuf},
};

use unicode_normalization::UnicodeNormalization;

use crate::error::MkoError;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProviderPath {
    pub canonical_file: PathBuf,
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
    let canonical_file = fs::canonicalize(file).map_err(|error| {
        MkoError::new(
            "file_unreadable",
            format!("cannot canonicalize {}: {error}", file.display()),
        )
    })?;
    if !fs::metadata(&canonical_file)
        .map_err(|error| MkoError::new("file_unreadable", error.to_string()))?
        .is_file()
    {
        return Err(MkoError::new(
            "file_unreadable",
            "capture input must be a regular file",
        ));
    }
    let relative = canonical_file.strip_prefix(provider_root).map_err(|_| {
        MkoError::new(
            "outside_allowed_root",
            "file is outside the configured provider root",
        )
    })?;
    let logical_path = normalized_logical_path(relative)?;
    reject_collisions(provider_root, relative)?;

    Ok(ProviderPath {
        canonical_file,
        logical_path,
    })
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

fn reject_collisions(root: &Path, relative: &Path) -> Result<(), MkoError> {
    let mut current = root.to_path_buf();
    for component in relative.components() {
        let Component::Normal(name) = component else {
            return Err(MkoError::new(
                "invalid_path",
                "logical paths must not contain traversal",
            ));
        };
        reject_component_collision(&current, name)?;
        current.push(name);
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

fn reject_component_collision(
    directory: &Path,
    expected: &std::ffi::OsStr,
) -> Result<(), MkoError> {
    let expected = expected
        .to_str()
        .ok_or_else(|| MkoError::new("invalid_path", "provider filename must be valid UTF-8"))?;
    let expected_key = collision_key(expected);
    let mut matches = Vec::new();
    for entry in fs::read_dir(directory)
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

fn collision_key(value: &str) -> String {
    value.nfc().collect::<String>().to_lowercase()
}

fn reject_windows_component(value: &str) -> Result<(), MkoError> {
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
