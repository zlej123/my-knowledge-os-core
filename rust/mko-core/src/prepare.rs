use std::{
    fs::{self, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    path::{Component, Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

use atomic_write_file::AtomicWriteFile;
use serde::{Deserialize, Serialize};

use crate::{
    CORE_VERSION,
    config::load_capture_config,
    error::MkoError,
    fingerprint::{FileSnapshot, fingerprint_file, fingerprint_open_file, validate_pdf_content},
    lock::AssetLock,
    model::{AssetStatus, Fingerprint},
    path_policy::provider_path,
    pdf::{
        EXTRACTOR_NAME, EXTRACTOR_VERSION, extract_pdf_pages_in_child, validate_extracted_pages,
    },
    registry::{mark_asset_extracted, read_asset},
};

const PROCESSOR_VERSION: &str = "source-v1";
const PROMPT_VERSION: &str = "codex-source-v1";
const TRUST: &str = "untrusted_document_text";
static NEXT_SNAPSHOT: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Debug)]
pub struct PrepareRequest {
    repository_root: PathBuf,
    local_config_path: Option<PathBuf>,
    asset_id: String,
    output: PathBuf,
    clear_stale_lock: bool,
}

impl PrepareRequest {
    pub fn new(
        repository_root: impl AsRef<Path>,
        asset_id: impl Into<String>,
        output: impl AsRef<Path>,
    ) -> Self {
        Self {
            repository_root: repository_root.as_ref().to_path_buf(),
            local_config_path: None,
            asset_id: asset_id.into(),
            output: output.as_ref().to_path_buf(),
            clear_stale_lock: false,
        }
    }

    pub fn with_local_config(mut self, path: impl AsRef<Path>) -> Self {
        self.local_config_path = Some(path.as_ref().to_path_buf());
        self
    }

    pub fn with_clear_stale_lock(mut self, clear: bool) -> Self {
        self.clear_stale_lock = clear;
        self
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct VersionedComponent {
    pub name: String,
    pub version: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PreparedSourceBundle {
    pub schema_version: u32,
    pub asset_id: String,
    pub source_id: String,
    pub fingerprint: Fingerprint,
    pub title_hint: String,
    pub logical_path: String,
    pub pages: Vec<String>,
    pub trust: String,
    pub extractor: VersionedComponent,
    pub core_version: String,
    pub processor_version: String,
    pub prompt_version: String,
}

pub fn prepare_source(
    request: PrepareRequest,
    worker_executable: &Path,
) -> Result<PreparedSourceBundle, MkoError> {
    prepare_source_with_extractor(request, |snapshot| {
        extract_pdf_pages_in_child(worker_executable, snapshot)
    })
}

pub fn prepare_source_with_extractor<F>(
    request: PrepareRequest,
    extractor: F,
) -> Result<PreparedSourceBundle, MkoError>
where
    F: FnOnce(&Path) -> Result<Vec<String>, MkoError>,
{
    let config = load_capture_config(
        &request.repository_root,
        request.local_config_path.as_deref(),
    )?;
    let source_id = source_id(&request.asset_id)?;
    let runtime = runtime_paths(
        &config.repository_root,
        &request.repository_root,
        &request.asset_id,
        &request.output,
    )?;
    let _lock = AssetLock::acquire(
        &config.repository_root,
        &request.asset_id,
        "mko source prepare",
        &crate::clock::SystemClock,
        request.clear_stale_lock,
    )?;
    let asset = read_asset(&config.repository_root, &request.asset_id)?;
    if !matches!(
        asset.asset_status,
        AssetStatus::Registered | AssetStatus::Extracted
    ) {
        return Err(MkoError::new(
            "invalid_state_transition",
            "source prepare requires a registered or extracted asset",
        ));
    }
    if asset.media_type != "application/pdf" {
        return Err(MkoError::new(
            "unsupported_format",
            "source prepare supports PDF assets only",
        ));
    }
    let candidate = config.provider_root.join(&asset.provider.locator);
    let mut provider = provider_path(&config.provider_root, &candidate)?.file;
    validate_pdf_content(&mut provider)?;
    let before = fingerprint_open_file(&mut provider)?;
    if before.fingerprint != asset.fingerprint {
        return Err(MkoError::new(
            "fingerprint_changed",
            "provider content no longer matches the registered asset",
        ));
    }
    let snapshot = Snapshot::copy_from(
        &runtime.snapshot_directory,
        &request.asset_id,
        &mut provider,
        &before,
    )?;
    let pages = extractor(&snapshot.path)?;
    validate_extracted_pages(&pages)?;
    snapshot.verify(&before)?;

    let retained_after = fingerprint_open_file(&mut provider)?;
    let mut reopened = provider_path(&config.provider_root, &candidate)?.file;
    let reopened_after = fingerprint_open_file(&mut reopened)?;
    if retained_after != before || reopened_after != before {
        return Err(MkoError::new(
            "fingerprint_changed",
            "PDF changed during extraction; prepared output was discarded",
        ));
    }

    let bundle = PreparedSourceBundle {
        schema_version: 1,
        asset_id: asset.id.clone(),
        source_id,
        fingerprint: asset.fingerprint.clone(),
        title_hint: asset.title.clone(),
        logical_path: asset.provider.locator.clone(),
        pages,
        trust: TRUST.into(),
        extractor: VersionedComponent {
            name: EXTRACTOR_NAME.into(),
            version: EXTRACTOR_VERSION.into(),
        },
        core_version: CORE_VERSION.into(),
        processor_version: PROCESSOR_VERSION.into(),
        prompt_version: PROMPT_VERSION.into(),
    };
    let mut bytes = serde_json::to_vec_pretty(&bundle)
        .map_err(|error| MkoError::new("bundle_invalid", error.to_string()))?;
    bytes.push(b'\n');
    let validated: PreparedSourceBundle = serde_json::from_slice(&bytes)
        .map_err(|error| MkoError::new("bundle_invalid", error.to_string()))?;
    if validated != bundle {
        return Err(MkoError::new(
            "bundle_invalid",
            "prepared source bundle failed deterministic validation",
        ));
    }
    write_runtime(&runtime.output, &bytes)?;
    let published: PreparedSourceBundle = serde_json::from_slice(
        &fs::read(&runtime.output)
            .map_err(|error| MkoError::new("runtime_write_failed", error.to_string()))?,
    )
    .map_err(|error| MkoError::new("bundle_invalid", error.to_string()))?;
    if published != bundle {
        return Err(MkoError::new(
            "bundle_invalid",
            "published source bundle failed validation",
        ));
    }
    if asset.asset_status == AssetStatus::Registered {
        mark_asset_extracted(&config.repository_root, &request.asset_id)?;
    }
    Ok(bundle)
}

fn source_id(asset_id: &str) -> Result<String, MkoError> {
    let hash = asset_id
        .strip_prefix("personal-asset-")
        .ok_or_else(|| MkoError::new("asset_id_invalid", "asset ID must be content addressed"))?;
    if hash.len() != 64
        || !hash
            .bytes()
            .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
    {
        return Err(MkoError::new(
            "asset_id_invalid",
            "asset ID must contain a full lowercase SHA-256 fingerprint",
        ));
    }
    Ok(format!("personal-source-{hash}"))
}

struct RuntimePaths {
    output: PathBuf,
    snapshot_directory: PathBuf,
}

fn runtime_paths(
    repository_root: &Path,
    requested_repository_root: &Path,
    asset_id: &str,
    requested: &Path,
) -> Result<RuntimePaths, MkoError> {
    let expected_relative = PathBuf::from(".knowledge-os")
        .join("runtime")
        .join("prepared")
        .join(format!("{asset_id}.json"));
    let requested_relative = if requested.is_absolute() {
        requested
            .strip_prefix(requested_repository_root)
            .or_else(|_| requested.strip_prefix(repository_root))
            .map_err(|_| runtime_output_error())?
    } else {
        requested
    };
    if requested_relative
        .components()
        .any(|component| component == Component::ParentDir)
    {
        return Err(runtime_output_error());
    }
    if requested_relative != expected_relative {
        return Err(runtime_output_error());
    }

    let knowledge = ensure_real_child_directory(repository_root, ".knowledge-os")?;
    let runtime = ensure_real_child_directory(&knowledge, "runtime")?;
    let prepared = ensure_real_child_directory(&runtime, "prepared")?;
    let snapshots = ensure_real_child_directory(&runtime, "snapshots")?;
    if !knowledge.starts_with(repository_root)
        || !runtime.starts_with(repository_root)
        || !prepared.starts_with(repository_root)
        || !snapshots.starts_with(repository_root)
    {
        return Err(runtime_path_error());
    }
    let output = prepared.join(format!("{asset_id}.json"));
    match fs::symlink_metadata(&output) {
        Ok(metadata) if metadata.file_type().is_file() => {}
        Ok(_) => return Err(runtime_output_error()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(_) => return Err(runtime_path_error()),
    }
    Ok(RuntimePaths {
        output,
        snapshot_directory: snapshots,
    })
}

fn ensure_real_child_directory(parent: &Path, name: &str) -> Result<PathBuf, MkoError> {
    let path = parent.join(name);
    match fs::symlink_metadata(&path) {
        Ok(metadata) if metadata.file_type().is_dir() => {}
        Ok(_) => return Err(runtime_path_error()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            match fs::create_dir(&path) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(_) => return Err(runtime_path_error()),
            }
            let metadata = fs::symlink_metadata(&path).map_err(|_| runtime_path_error())?;
            if !metadata.file_type().is_dir() {
                return Err(runtime_path_error());
            }
        }
        Err(_) => return Err(runtime_path_error()),
    }
    let canonical = fs::canonicalize(&path).map_err(|_| runtime_path_error())?;
    let canonical_parent = fs::canonicalize(parent).map_err(|_| runtime_path_error())?;
    if canonical.parent() != Some(canonical_parent.as_path()) {
        return Err(runtime_path_error());
    }
    Ok(canonical)
}

fn runtime_output_error() -> MkoError {
    MkoError::new(
        "runtime_output_invalid",
        "output must be .knowledge-os/runtime/prepared/<asset-id>.json for the requested asset",
    )
}

fn runtime_path_error() -> MkoError {
    MkoError::new(
        "runtime_path_invalid",
        "runtime directories must be real directories inside the repository",
    )
}

fn write_runtime(path: &Path, bytes: &[u8]) -> Result<(), MkoError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_file() => {}
        Ok(_) => {
            return Err(MkoError::new(
                "runtime_destination_invalid",
                "runtime destination exists but is not a regular file",
            ));
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(MkoError::new("runtime_write_failed", error.to_string())),
    }
    let mut file = AtomicWriteFile::open(path)
        .map_err(|error| MkoError::new("runtime_write_failed", error.to_string()))?;
    file.write_all(bytes)
        .map_err(|error| MkoError::new("runtime_write_failed", error.to_string()))?;
    file.commit()
        .map_err(|error| MkoError::new("runtime_write_failed", error.to_string()))
}

#[derive(Debug)]
struct Snapshot {
    path: PathBuf,
}

impl Snapshot {
    fn copy_from(
        directory: &Path,
        asset_id: &str,
        provider: &mut cap_std::fs::File,
        expected: &FileSnapshot,
    ) -> Result<Self, MkoError> {
        Self::copy_from_with_hook(directory, asset_id, provider, expected, |_| {})
    }

    fn copy_from_with_hook<F>(
        directory: &Path,
        asset_id: &str,
        provider: &mut cap_std::fs::File,
        expected: &FileSnapshot,
        mut after_chunk: F,
    ) -> Result<Self, MkoError>
    where
        F: FnMut(usize),
    {
        let path = directory.join(format!(
            ".{asset_id}.{}.{}.pdf",
            std::process::id(),
            NEXT_SNAPSHOT.fetch_add(1, Ordering::Relaxed)
        ));
        let mut snapshot = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .map_err(|error| MkoError::new("runtime_write_failed", error.to_string()))?;
        provider
            .seek(SeekFrom::Start(0))
            .map_err(|error| MkoError::new("file_unreadable", error.to_string()))?;
        let result: Result<(), MkoError> = (|| {
            let mut buffer = [0_u8; 64 * 1024];
            let mut chunk = 0_usize;
            loop {
                let read = provider
                    .read(&mut buffer)
                    .map_err(|error| MkoError::new("file_unreadable", error.to_string()))?;
                if read == 0 {
                    break;
                }
                snapshot
                    .write_all(&buffer[..read])
                    .map_err(|error| MkoError::new("runtime_write_failed", error.to_string()))?;
                chunk += 1;
                after_chunk(chunk);
            }
            snapshot
                .sync_all()
                .map_err(|error| MkoError::new("runtime_write_failed", error.to_string()))
        })();
        provider
            .seek(SeekFrom::Start(0))
            .map_err(|error| MkoError::new("file_unreadable", error.to_string()))?;
        if let Err(error) = result {
            let _ = fs::remove_file(&path);
            return Err(error);
        }
        let result = Self { path };
        result.verify(expected)?;
        Ok(result)
    }

    fn verify(&self, expected: &FileSnapshot) -> Result<(), MkoError> {
        let fingerprint = fingerprint_file(&self.path)?;
        let size_bytes = fs::metadata(&self.path)
            .map_err(|error| MkoError::new("runtime_write_failed", error.to_string()))?
            .len();
        if fingerprint != expected.fingerprint || size_bytes != expected.size_bytes {
            return Err(MkoError::new(
                "fingerprint_changed",
                "runtime PDF snapshot does not match the registered fingerprint and size",
            ));
        }
        Ok(())
    }
}

impl Drop for Snapshot {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

#[cfg(test)]
mod tests {
    use std::{fs, sync::Arc};

    use cap_std::{ambient_authority, fs::Dir};

    use super::Snapshot;
    use crate::fingerprint::fingerprint_open_file;

    #[test]
    fn snapshot_copy_rejects_mutation_that_is_restored_before_final_provider_check() {
        let root = tempfile::tempdir().unwrap();
        let source_path = root.path().join("paper.pdf");
        let original = Arc::new(vec![b'a'; 4 * 64 * 1024]);
        fs::write(&source_path, &*original).unwrap();
        let directory = Dir::open_ambient_dir(root.path(), ambient_authority()).unwrap();
        let mut provider = directory.open("paper.pdf").unwrap();
        let expected = fingerprint_open_file(&mut provider).unwrap();
        let prepared = root.path().join("prepared");
        fs::create_dir(&prepared).unwrap();
        let output = prepared.clone();
        let source_for_hook = source_path.clone();
        let original_for_hook = Arc::clone(&original);

        let error = Snapshot::copy_from_with_hook(
            &output,
            &format!("personal-asset-{}", "a".repeat(64)),
            &mut provider,
            &expected,
            move |chunk| {
                if chunk == 1 {
                    fs::write(&source_for_hook, vec![b'b'; original_for_hook.len()]).unwrap();
                } else if chunk == 2 {
                    fs::write(&source_for_hook, &*original_for_hook).unwrap();
                }
            },
        )
        .unwrap_err();

        assert_eq!(error.code(), "fingerprint_changed");
        assert_eq!(fs::read(source_path).unwrap(), *original);
        assert_eq!(fs::read_dir(prepared).unwrap().count(), 0);
    }
}
