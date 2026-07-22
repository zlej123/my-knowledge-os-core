use std::{fs, path::Path};

use crate::{
    atomic::{AtomicWriteResult, write_new},
    config_v2::KnowledgeConfigV2,
    error::MkoError,
};

const OWNED_DIRECTORIES: &[&str] = &[
    "assets",
    "assets/registry",
    "sources",
    "knowledge",
    "reviews",
    "views",
    "views/records",
    ".mko",
    "recovery",
    "recovery/manual-edits",
];

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ScaffoldOutcomeV2 {
    Created,
    Repaired,
    Existing,
}

pub fn scaffold_personal_kb_v2(repository_root: &Path) -> Result<ScaffoldOutcomeV2, MkoError> {
    ensure_root_directory(repository_root)?;
    let marker = repository_root.join("knowledge-os.yaml");
    let marker_exists = match fs::symlink_metadata(&marker) {
        Ok(metadata) if metadata.file_type().is_file() && !metadata.file_type().is_symlink() => {
            true
        }
        Ok(_) => {
            return Err(MkoError::new(
                "kb_config_invalid",
                "knowledge-os.yaml must be a regular file",
            ));
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
        Err(error) => return Err(MkoError::new("kb_config_unreadable", error.to_string())),
    };

    if !marker_exists && directory_has_entries(repository_root)? {
        return Err(MkoError::new(
            "kb_destination_not_empty",
            "choose an empty directory or an existing v0.3 Personal KB",
        ));
    }

    if marker_exists {
        KnowledgeConfigV2::read(repository_root)?;
    }

    let mut repaired = false;
    for relative in OWNED_DIRECTORIES {
        repaired |= ensure_owned_directory(repository_root, relative)?;
    }

    let desired = KnowledgeConfigV2::personal_default().render()?;
    let marker_result = write_new(&marker, &desired, |path| {
        let existing = fs::read(path)
            .map_err(|error| MkoError::new("kb_config_unreadable", error.to_string()))?;
        if existing == desired {
            Ok(())
        } else {
            Err(MkoError::new(
                "kb_config_conflict",
                "knowledge-os.yaml differs from the v0.3 Personal contract",
            ))
        }
    })?;

    Ok(match (marker_result, repaired) {
        (AtomicWriteResult::Created, _) => ScaffoldOutcomeV2::Created,
        (AtomicWriteResult::Existing, true) => ScaffoldOutcomeV2::Repaired,
        (AtomicWriteResult::Existing, false) => ScaffoldOutcomeV2::Existing,
    })
}

fn ensure_root_directory(path: &Path) -> Result<(), MkoError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => Ok(()),
        Ok(_) => Err(MkoError::new(
            "kb_destination_invalid",
            "the KB destination must be a real directory",
        )),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => fs::create_dir(path)
            .map_err(|error| MkoError::new("kb_create_failed", error.to_string())),
        Err(error) => Err(MkoError::new("kb_create_failed", error.to_string())),
    }
}

fn directory_has_entries(path: &Path) -> Result<bool, MkoError> {
    let mut entries = fs::read_dir(path)
        .map_err(|error| MkoError::new("kb_destination_invalid", error.to_string()))?;
    Ok(entries
        .next()
        .transpose()
        .map_err(|error| MkoError::new("kb_destination_invalid", error.to_string()))?
        .is_some())
}

fn ensure_owned_directory(root: &Path, relative: &str) -> Result<bool, MkoError> {
    let path = root.join(relative);
    match fs::symlink_metadata(&path) {
        Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => Ok(false),
        Ok(_) => Err(MkoError::new(
            "kb_path_invalid",
            format!("managed path {relative} is not a real directory"),
        )),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            fs::create_dir(&path)
                .map_err(|error| MkoError::new("kb_create_failed", error.to_string()))?;
            Ok(true)
        }
        Err(error) => Err(MkoError::new("kb_create_failed", error.to_string())),
    }
}
