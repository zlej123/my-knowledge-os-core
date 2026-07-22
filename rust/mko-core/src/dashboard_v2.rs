use std::{fs, path::Path};

use crate::{
    atomic::{AtomicWriteResult, write_new},
    clock::SystemClock,
    config_v2::KnowledgeConfigV2,
    error::MkoError,
    lock::{RepositoryMutationLock, StaleRepositoryLockPolicy},
};

const GENERATED_FILES: &[(&str, &str)] = &[
    (
        "HOME.md",
        r#"---
type: dashboard
generated_by: my-knowledge-os
---

# My Knowledge OS

## 검토 대기

![[views/review-queue.base]]

## 승인된 지식

![[views/knowledge-library.base]]

터미널에서는 `mko queue`로 같은 검토 대기열을 확인할 수 있습니다.
"#,
    ),
    (
        "views/review-queue.base",
        r#"filters:
  and:
    - file.inFolder("views/records")
    - 'derived_state != "approved"'
views:
  - type: table
    name: Review Queue
    order:
      - title
      - record_type
      - derived_state
      - domain
      - current_revision
"#,
    ),
    (
        "views/knowledge-library.base",
        r#"filters:
  and:
    - file.inFolder("views/records")
    - 'record_type == "knowledge"'
    - 'derived_state == "approved"'
views:
  - type: table
    name: Knowledge Library
    order:
      - title
      - domain
      - tags
      - current_revision
"#,
    ),
];

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DashboardOutcomeV2 {
    Created,
    Existing,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DashboardResultV2 {
    pub outcome: DashboardOutcomeV2,
    pub generated_files: Vec<String>,
}

pub fn ensure_dashboard_v2(repository_root: &Path) -> Result<DashboardResultV2, MkoError> {
    KnowledgeConfigV2::read(repository_root)?;
    let _lock = RepositoryMutationLock::acquire(
        repository_root,
        "v2 dashboard ensure",
        &SystemClock,
        StaleRepositoryLockPolicy::Preserve,
    )?;
    require_real_directory(&repository_root.join("views"))?;
    let mut created = false;
    let mut generated_files = Vec::new();
    for (relative, expected) in GENERATED_FILES {
        let path = repository_root.join(relative);
        let expected = expected.as_bytes();
        let outcome = write_new(&path, expected, |existing| {
            let metadata = fs::symlink_metadata(existing)
                .map_err(|error| MkoError::new("dashboard_drift", error.to_string()))?;
            if !metadata.is_file() || metadata.file_type().is_symlink() {
                return Err(MkoError::new(
                    "dashboard_drift",
                    format!("generated dashboard path {relative} is not a regular file"),
                ));
            }
            let bytes = fs::read(existing)
                .map_err(|error| MkoError::new("dashboard_drift", error.to_string()))?;
            if bytes == *expected {
                Ok(())
            } else {
                Err(MkoError::new(
                    "dashboard_drift",
                    format!(
                        "generated dashboard {relative} was edited; preserve it and choose an explicit repair"
                    ),
                ))
            }
        })?;
        created |= outcome == AtomicWriteResult::Created;
        generated_files.push((*relative).to_owned());
    }
    Ok(DashboardResultV2 {
        outcome: if created {
            DashboardOutcomeV2::Created
        } else {
            DashboardOutcomeV2::Existing
        },
        generated_files,
    })
}

fn require_real_directory(path: &Path) -> Result<(), MkoError> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|error| MkoError::new("dashboard_path_invalid", error.to_string()))?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err(MkoError::new(
            "dashboard_path_invalid",
            "dashboard parent must be a real directory",
        ));
    }
    Ok(())
}
