use std::{
    io::{Read, Seek, SeekFrom, Write},
    path::{Component, Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

use cap_std::{
    ambient_authority,
    fs::{Dir, File, OpenOptions, OpenOptionsExt},
};
use serde::{Deserialize, Serialize};

use crate::{
    config::{CaptureConfig, load_capture_config},
    context::ResolvedPersonalContext,
    error::MkoError,
    fingerprint::{FileSnapshot, fingerprint_open_file, validate_pdf_content},
    lock::AssetLock,
    model::{AssetStatus, Fingerprint},
    path_policy::provider_path,
    pdf::{
        EXTRACTOR_NAME, EXTRACTOR_VERSION, extract_pdf_pages_in_child, validate_extracted_pages,
    },
    registry::{mark_asset_extracted, read_asset},
    version::KNOWLEDGE_CONTRACT_VERSION,
};

pub const PROCESSOR_VERSION: &str = "source-v1";
pub const PROMPT_VERSION: &str = "codex-source-v1";
pub const TRUST: &str = "untrusted_document_text";
const MAX_PREPARED_BUNDLE_BYTES: u64 = 128 * 1024 * 1024;
static NEXT_SNAPSHOT: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Debug)]
pub struct PrepareRequest {
    repository_root: PathBuf,
    local_config_path: Option<PathBuf>,
    resolved_context: Option<ResolvedPersonalContext>,
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
            resolved_context: None,
            asset_id: asset_id.into(),
            output: output.as_ref().to_path_buf(),
            clear_stale_lock: false,
        }
    }

    pub fn with_local_config(mut self, path: impl AsRef<Path>) -> Self {
        self.local_config_path = Some(path.as_ref().to_path_buf());
        self
    }

    pub fn with_resolved_context(mut self, context: ResolvedPersonalContext) -> Self {
        self.resolved_context = Some(context);
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

pub fn load_prepared_source_bundle(
    repository_root: &Path,
    requested: &Path,
) -> Result<PreparedSourceBundle, MkoError> {
    load_prepared_source_bundle_with_before_open(repository_root, requested, || {})
}

fn load_prepared_source_bundle_with_before_open<F>(
    repository_root: &Path,
    requested: &Path,
    before_open: F,
) -> Result<PreparedSourceBundle, MkoError>
where
    F: FnOnce(),
{
    let requested_repository_root = repository_root.to_path_buf();
    let repository_root = crate::path_policy::canonical_directory(
        &requested_repository_root,
        "repository_root_invalid",
    )?;
    let asset_id = requested
        .file_stem()
        .and_then(|value| value.to_str())
        .ok_or_else(runtime_output_error)?;
    source_id(asset_id).map_err(|_| runtime_output_error())?;
    let runtime = runtime_paths(
        &repository_root,
        &requested_repository_root,
        asset_id,
        requested,
    )?;
    before_open();
    let mut options = OpenOptions::new();
    options.read(true);
    configure_bundle_nofollow(&mut options);
    let file = runtime
        .prepared
        .open_with(&runtime.output_name, &options)
        .map_err(|_| runtime_publication_error())?;
    let metadata = file.metadata().map_err(|_| runtime_publication_error())?;
    if !metadata.is_file()
        || metadata.file_type().is_symlink()
        || metadata.len() > MAX_PREPARED_BUNDLE_BYTES
    {
        return Err(runtime_publication_error());
    }
    let mut bytes = Vec::new();
    file.take(MAX_PREPARED_BUNDLE_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| runtime_publication_error())?;
    if bytes.len() as u64 > MAX_PREPARED_BUNDLE_BYTES {
        return Err(MkoError::new(
            "bundle_invalid",
            "prepared Source bundle exceeds its bounded runtime transport",
        ));
    }
    let bundle: PreparedSourceBundle = serde_json::from_slice(&bytes)
        .map_err(|error| MkoError::new("bundle_invalid", error.to_string()))?;
    validate_prepared_bundle_contract(&bundle, asset_id)?;
    Ok(bundle)
}

#[cfg(target_os = "linux")]
fn configure_bundle_nofollow(options: &mut OpenOptions) {
    const O_NONBLOCK: i32 = 0x800;
    const O_NOFOLLOW: i32 = 0x20_000;
    options.custom_flags(O_NOFOLLOW | O_NONBLOCK);
}

#[cfg(target_os = "macos")]
fn configure_bundle_nofollow(options: &mut OpenOptions) {
    const O_NONBLOCK: i32 = 0x4;
    const O_NOFOLLOW: i32 = 0x100;
    options.custom_flags(O_NOFOLLOW | O_NONBLOCK);
}

#[cfg(windows)]
fn configure_bundle_nofollow(options: &mut OpenOptions) {
    const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
    options.custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
}

#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
fn configure_bundle_nofollow(_options: &mut OpenOptions) {}

fn validate_prepared_bundle_contract(
    bundle: &PreparedSourceBundle,
    asset_id: &str,
) -> Result<(), MkoError> {
    let expected_source_id = source_id(asset_id)?;
    let hash = asset_id
        .strip_prefix("personal-asset-")
        .ok_or_else(|| MkoError::new("bundle_invalid", "invalid prepared Asset ID"))?;
    if bundle.schema_version != 1
        || bundle.asset_id != asset_id
        || bundle.source_id != expected_source_id
        || bundle.fingerprint.method != "sha256"
        || bundle.fingerprint.value != format!("sha256:{hash}")
        || bundle.title_hint.is_empty()
        || bundle.logical_path.is_empty()
        || bundle.trust != TRUST
        || bundle.extractor.name != EXTRACTOR_NAME
        || bundle.extractor.version != EXTRACTOR_VERSION
        || bundle.core_version != KNOWLEDGE_CONTRACT_VERSION
        || bundle.processor_version != PROCESSOR_VERSION
        || bundle.prompt_version != PROMPT_VERSION
    {
        return Err(MkoError::new(
            "bundle_invalid",
            "prepared Source bundle does not match the canonical Core contract",
        ));
    }
    validate_extracted_pages(&bundle.pages)?;
    Ok(())
}

pub fn prepare_source(
    request: PrepareRequest,
    worker_executable: &Path,
) -> Result<PreparedSourceBundle, MkoError> {
    prepare_source_with_extractor(request, |snapshot, expected| {
        extract_pdf_pages_in_child(worker_executable, snapshot, expected)
    })
}

pub fn prepare_source_with_extractor<F>(
    request: PrepareRequest,
    extractor: F,
) -> Result<PreparedSourceBundle, MkoError>
where
    F: FnOnce(File, &FileSnapshot) -> Result<Vec<String>, MkoError>,
{
    let config = match request.resolved_context.as_ref() {
        Some(context) => CaptureConfig::from_resolved_context(context)?,
        None => load_capture_config(
            &request.repository_root,
            request.local_config_path.as_deref(),
        )?,
    };
    let requested_repository_root = request
        .resolved_context
        .as_ref()
        .map(|context| context.repository_root.as_path())
        .unwrap_or(&request.repository_root);
    let source_id = source_id(&request.asset_id)?;
    let runtime = runtime_paths(
        &config.repository_root,
        requested_repository_root,
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
        &runtime.snapshots,
        &request.asset_id,
        &mut provider,
        &before,
    )?;
    let pages = extractor(snapshot.clone_file()?, &before)?;
    validate_extracted_pages(&pages)?;

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
        core_version: KNOWLEDGE_CONTRACT_VERSION.into(),
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
    let staged = write_runtime(&runtime.prepared, &runtime.output_name, &bytes)?;
    let published: PreparedSourceBundle = serde_json::from_slice(&staged)
        .map_err(|error| MkoError::new("bundle_invalid", error.to_string()))?;
    if published != bundle {
        return Err(MkoError::new(
            "bundle_invalid",
            "published source bundle failed validation",
        ));
    }
    verify_public_runtime_bundle(&runtime.runtime, &runtime.output_name, &bytes, &bundle)?;
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
    runtime: Dir,
    prepared: Dir,
    snapshots: Dir,
    output_name: PathBuf,
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

    let repository = Dir::open_ambient_dir(repository_root, ambient_authority())
        .map_err(|_| runtime_path_error())?;
    let knowledge = ensure_real_child_directory(&repository, ".knowledge-os")?;
    let runtime = ensure_real_child_directory(&knowledge, "runtime")?;
    let prepared = ensure_real_child_directory(&runtime, "prepared")?;
    let snapshots = ensure_real_child_directory(&runtime, "snapshots")?;
    let output_name = PathBuf::from(format!("{asset_id}.json"));
    match prepared.symlink_metadata(&output_name) {
        Ok(metadata) if metadata.file_type().is_file() => {}
        Ok(_) => return Err(runtime_output_error()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(_) => return Err(runtime_path_error()),
    }
    Ok(RuntimePaths {
        runtime,
        prepared,
        snapshots,
        output_name,
    })
}

fn verify_public_runtime_bundle(
    runtime: &Dir,
    output_name: &Path,
    expected_bytes: &[u8],
    expected_bundle: &PreparedSourceBundle,
) -> Result<(), MkoError> {
    let public_path = PathBuf::from("prepared").join(output_name);
    let file = runtime
        .open(&public_path)
        .map_err(|_| runtime_publication_error())?;
    let byte_limit = expected_bytes
        .len()
        .checked_add(1)
        .and_then(|limit| u64::try_from(limit).ok())
        .ok_or_else(runtime_publication_error)?;
    let mut published_bytes = Vec::with_capacity(expected_bytes.len());
    file.take(byte_limit)
        .read_to_end(&mut published_bytes)
        .map_err(|_| runtime_publication_error())?;
    if published_bytes != expected_bytes {
        return Err(runtime_publication_error());
    }
    let published: PreparedSourceBundle =
        serde_json::from_slice(&published_bytes).map_err(|_| runtime_publication_error())?;
    if published.schema_version != 1
        || published.asset_id != expected_bundle.asset_id
        || published.fingerprint != expected_bundle.fingerprint
        || published != *expected_bundle
    {
        return Err(runtime_publication_error());
    }
    Ok(())
}

fn ensure_real_child_directory(parent: &Dir, name: &str) -> Result<Dir, MkoError> {
    match parent.symlink_metadata(name) {
        Ok(metadata) if metadata.file_type().is_dir() => {}
        Ok(_) => return Err(runtime_path_error()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            match parent.create_dir(name) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(_) => return Err(runtime_path_error()),
            }
            let metadata = parent
                .symlink_metadata(name)
                .map_err(|_| runtime_path_error())?;
            if !metadata.file_type().is_dir() {
                return Err(runtime_path_error());
            }
        }
        Err(_) => return Err(runtime_path_error()),
    }
    let child = parent.open_dir(name).map_err(|_| runtime_path_error())?;
    let metadata = parent
        .symlink_metadata(name)
        .map_err(|_| runtime_path_error())?;
    if !metadata.file_type().is_dir() {
        return Err(runtime_path_error());
    }
    Ok(child)
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

fn runtime_publication_error() -> MkoError {
    MkoError::new(
        "runtime_publication_invalid",
        "public prepared bundle does not match the atomically published bundle",
    )
}

fn write_runtime(directory: &Dir, name: &Path, bytes: &[u8]) -> Result<Vec<u8>, MkoError> {
    match directory.symlink_metadata(name) {
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
    let temporary = PathBuf::from(format!(
        ".{}.{}.{}.tmp",
        name.to_string_lossy(),
        std::process::id(),
        NEXT_SNAPSHOT.fetch_add(1, Ordering::Relaxed)
    ));
    let result = (|| {
        let mut options = OpenOptions::new();
        options.read(true).write(true).create_new(true);
        let mut file = directory
            .open_with(&temporary, &options)
            .map_err(|error| MkoError::new("runtime_write_failed", error.to_string()))?;
        file.write_all(bytes)
            .map_err(|error| MkoError::new("runtime_write_failed", error.to_string()))?;
        file.sync_all()
            .map_err(|error| MkoError::new("runtime_write_failed", error.to_string()))?;
        file.seek(SeekFrom::Start(0))
            .map_err(|error| MkoError::new("runtime_write_failed", error.to_string()))?;
        let mut staged = Vec::with_capacity(bytes.len());
        file.read_to_end(&mut staged)
            .map_err(|error| MkoError::new("runtime_write_failed", error.to_string()))?;
        if staged != bytes {
            return Err(MkoError::new(
                "runtime_write_failed",
                "staged runtime bundle failed byte-for-byte validation",
            ));
        }
        drop(file);
        directory
            .rename(&temporary, directory, name)
            .map_err(|error| MkoError::new("runtime_write_failed", error.to_string()))?;
        Ok(staged)
    })();
    if result.is_err() {
        let _ = directory.remove_file(&temporary);
    }
    result
}

struct Snapshot {
    directory: Dir,
    name: PathBuf,
    file: Option<File>,
}

impl Snapshot {
    fn copy_from(
        directory: &Dir,
        asset_id: &str,
        provider: &mut cap_std::fs::File,
        expected: &FileSnapshot,
    ) -> Result<Self, MkoError> {
        Self::copy_from_with_hook(directory, asset_id, provider, expected, |_| {})
    }

    fn copy_from_with_hook<F>(
        directory: &Dir,
        asset_id: &str,
        provider: &mut cap_std::fs::File,
        expected: &FileSnapshot,
        mut after_chunk: F,
    ) -> Result<Self, MkoError>
    where
        F: FnMut(usize),
    {
        let name = PathBuf::from(format!(
            ".{asset_id}.{}.{}.pdf",
            std::process::id(),
            NEXT_SNAPSHOT.fetch_add(1, Ordering::Relaxed)
        ));
        let mut options = OpenOptions::new();
        options.read(true).write(true).create_new(true);
        let mut snapshot = directory
            .open_with(&name, &options)
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
            drop(snapshot);
            let _ = directory.remove_file(&name);
            return Err(error);
        }
        let retained_directory = match directory.try_clone() {
            Ok(directory) => directory,
            Err(error) => {
                drop(snapshot);
                let _ = directory.remove_file(&name);
                return Err(MkoError::new("runtime_write_failed", error.to_string()));
            }
        };
        let result = Self {
            directory: retained_directory,
            name,
            file: Some(snapshot),
        };
        result.verify(expected)?;
        Ok(result)
    }

    fn verify(&self, expected: &FileSnapshot) -> Result<(), MkoError> {
        let mut file = self
            .file
            .as_ref()
            .ok_or_else(|| MkoError::new("runtime_write_failed", "snapshot handle is missing"))?
            .try_clone()
            .map_err(|error| MkoError::new("runtime_write_failed", error.to_string()))?;
        let actual = fingerprint_open_file(&mut file)?;
        if actual.fingerprint != expected.fingerprint || actual.size_bytes != expected.size_bytes {
            return Err(MkoError::new(
                "fingerprint_changed",
                "runtime PDF snapshot does not match the registered fingerprint and size",
            ));
        }
        Ok(())
    }

    fn clone_file(&self) -> Result<File, MkoError> {
        self.file
            .as_ref()
            .ok_or_else(|| MkoError::new("runtime_write_failed", "snapshot handle is missing"))
            .and_then(|file| {
                file.try_clone()
                    .map_err(|error| MkoError::new("runtime_write_failed", error.to_string()))
            })
    }
}

impl std::fmt::Debug for Snapshot {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Snapshot")
            .field("name", &self.name)
            .finish_non_exhaustive()
    }
}

impl Drop for Snapshot {
    fn drop(&mut self) {
        drop(self.file.take());
        let _ = self.directory.remove_file(&self.name);
    }
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        sync::{Arc, mpsc},
        thread,
        time::Duration,
    };

    use cap_std::{ambient_authority, fs::Dir};

    use super::{
        PROCESSOR_VERSION, PROMPT_VERSION, PreparedSourceBundle, Snapshot, TRUST,
        VersionedComponent, load_prepared_source_bundle_with_before_open, runtime_paths,
        write_runtime,
    };
    use crate::{
        fingerprint::fingerprint_open_file,
        model::Fingerprint,
        pdf::{EXTRACTOR_NAME, EXTRACTOR_VERSION},
        version::KNOWLEDGE_CONTRACT_VERSION,
    };

    fn bundle(asset_id: &str, title: &str) -> PreparedSourceBundle {
        let hash = asset_id.strip_prefix("personal-asset-").unwrap();
        PreparedSourceBundle {
            schema_version: 1,
            asset_id: asset_id.into(),
            source_id: asset_id.replacen("asset", "source", 1),
            fingerprint: Fingerprint {
                method: "sha256".into(),
                value: format!("sha256:{hash}"),
            },
            title_hint: title.into(),
            logical_path: "paper.pdf".into(),
            pages: vec!["Fixture page".into()],
            trust: TRUST.into(),
            extractor: VersionedComponent {
                name: EXTRACTOR_NAME.into(),
                version: EXTRACTOR_VERSION.into(),
            },
            core_version: KNOWLEDGE_CONTRACT_VERSION.into(),
            processor_version: PROCESSOR_VERSION.into(),
            prompt_version: PROMPT_VERSION.into(),
        }
    }

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
        let output = Dir::open_ambient_dir(&prepared, ambient_authority()).unwrap();
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

    #[cfg(unix)]
    #[test]
    fn retained_snapshot_directory_handle_prevents_post_validation_escape() {
        let root = tempfile::tempdir().unwrap();
        let repository = root.path().join("repository");
        let outside = root.path().join("outside");
        fs::create_dir(&repository).unwrap();
        fs::create_dir(&outside).unwrap();
        let asset_id = format!("personal-asset-{}", "a".repeat(64));
        let output = repository
            .join(".knowledge-os/runtime/prepared")
            .join(format!("{asset_id}.json"));
        let runtime = runtime_paths(&repository, &repository, &asset_id, &output).unwrap();
        let retained = repository.join(".knowledge-os/runtime/snapshots-retained");
        fs::rename(
            repository.join(".knowledge-os/runtime/snapshots"),
            &retained,
        )
        .unwrap();
        std::os::unix::fs::symlink(&outside, repository.join(".knowledge-os/runtime/snapshots"))
            .unwrap();
        let provider_root = root.path().join("provider");
        fs::create_dir(&provider_root).unwrap();
        fs::write(provider_root.join("paper.pdf"), b"%PDF-retained bytes").unwrap();
        let provider_dir = Dir::open_ambient_dir(&provider_root, ambient_authority()).unwrap();
        let mut provider = provider_dir.open("paper.pdf").unwrap();
        let expected = fingerprint_open_file(&mut provider).unwrap();

        let snapshot =
            Snapshot::copy_from(&runtime.snapshots, &asset_id, &mut provider, &expected).unwrap();

        assert_eq!(fs::read_dir(&outside).unwrap().count(), 0);
        assert_eq!(fs::read_dir(&retained).unwrap().count(), 1);
        drop(snapshot);
        assert_eq!(fs::read_dir(&retained).unwrap().count(), 0);
    }

    #[cfg(unix)]
    #[test]
    fn retained_prepared_directory_handle_prevents_post_validation_escape() {
        let root = tempfile::tempdir().unwrap();
        let repository = root.path().join("repository");
        let outside = root.path().join("outside");
        fs::create_dir(&repository).unwrap();
        fs::create_dir(&outside).unwrap();
        let asset_id = format!("personal-asset-{}", "b".repeat(64));
        let output = repository
            .join(".knowledge-os/runtime/prepared")
            .join(format!("{asset_id}.json"));
        let runtime = runtime_paths(&repository, &repository, &asset_id, &output).unwrap();
        let retained = repository.join(".knowledge-os/runtime/prepared-retained");
        fs::rename(repository.join(".knowledge-os/runtime/prepared"), &retained).unwrap();
        std::os::unix::fs::symlink(&outside, repository.join(".knowledge-os/runtime/prepared"))
            .unwrap();

        write_runtime(&runtime.prepared, &runtime.output_name, b"retained bundle").unwrap();

        assert_eq!(fs::read_dir(&outside).unwrap().count(), 0);
        assert_eq!(
            fs::read(retained.join(&runtime.output_name)).unwrap(),
            b"retained bundle"
        );
    }

    #[cfg(unix)]
    #[test]
    fn bundle_loader_uses_retained_prepared_directory_after_public_directory_swap() {
        let root = tempfile::tempdir().unwrap();
        let repository = root.path().join("repository");
        let outside = root.path().join("outside");
        fs::create_dir(&repository).unwrap();
        fs::create_dir(&outside).unwrap();
        let asset_id = format!("personal-asset-{}", "c".repeat(64));
        let output = repository
            .join(".knowledge-os/runtime/prepared")
            .join(format!("{asset_id}.json"));
        let runtime = runtime_paths(&repository, &repository, &asset_id, &output).unwrap();
        let expected = bundle(&asset_id, "Retained bundle");
        write_runtime(
            &runtime.prepared,
            &runtime.output_name,
            &serde_json::to_vec(&expected).unwrap(),
        )
        .unwrap();
        let attacker = bundle(&asset_id, "Attacker bundle");
        fs::write(
            outside.join(&runtime.output_name),
            serde_json::to_vec(&attacker).unwrap(),
        )
        .unwrap();
        let prepared = repository.join(".knowledge-os/runtime/prepared");
        let retained = repository.join(".knowledge-os/runtime/prepared-retained");

        let loaded = load_prepared_source_bundle_with_before_open(&repository, &output, || {
            fs::rename(&prepared, &retained).unwrap();
            std::os::unix::fs::symlink(&outside, &prepared).unwrap();
        })
        .unwrap();

        assert_eq!(loaded, expected);
        assert_ne!(loaded, attacker);
    }

    #[cfg(unix)]
    #[test]
    fn bundle_loader_rejects_entry_symlink_swapped_after_path_validation() {
        let root = tempfile::tempdir().unwrap();
        let repository = root.path().join("repository");
        let outside = root.path().join("outside");
        fs::create_dir(&repository).unwrap();
        fs::create_dir(&outside).unwrap();
        let asset_id = format!("personal-asset-{}", "d".repeat(64));
        let output = repository
            .join(".knowledge-os/runtime/prepared")
            .join(format!("{asset_id}.json"));
        let runtime = runtime_paths(&repository, &repository, &asset_id, &output).unwrap();
        let expected = bundle(&asset_id, "Expected bundle");
        write_runtime(
            &runtime.prepared,
            &runtime.output_name,
            &serde_json::to_vec(&expected).unwrap(),
        )
        .unwrap();
        let attacker_path = outside.join("attacker.json");
        fs::write(
            &attacker_path,
            serde_json::to_vec(&bundle(&asset_id, "Attacker bundle")).unwrap(),
        )
        .unwrap();
        let saved = output.with_extension("saved");

        let error = load_prepared_source_bundle_with_before_open(&repository, &output, || {
            fs::rename(&output, &saved).unwrap();
            std::os::unix::fs::symlink(&attacker_path, &output).unwrap();
        })
        .unwrap_err();

        assert_eq!(error.code(), "runtime_publication_invalid");
    }

    #[cfg(unix)]
    #[test]
    fn bundle_loader_rejects_entry_fifo_swapped_after_path_validation_without_blocking() {
        let root = tempfile::tempdir().unwrap();
        let repository = root.path().join("repository");
        fs::create_dir(&repository).unwrap();
        let asset_id = format!("personal-asset-{}", "e".repeat(64));
        let output = repository
            .join(".knowledge-os/runtime/prepared")
            .join(format!("{asset_id}.json"));
        let runtime = runtime_paths(&repository, &repository, &asset_id, &output).unwrap();
        write_runtime(
            &runtime.prepared,
            &runtime.output_name,
            &serde_json::to_vec(&bundle(&asset_id, "Expected bundle")).unwrap(),
        )
        .unwrap();
        let saved = output.with_extension("saved");
        let repository_for_worker = repository.clone();
        let output_for_worker = output.clone();
        let fifo = output.clone();
        let (sender, receiver) = mpsc::channel();

        let worker = thread::spawn(move || {
            let result = load_prepared_source_bundle_with_before_open(
                &repository_for_worker,
                &output_for_worker,
                || {
                    fs::rename(&output_for_worker, &saved).unwrap();
                    assert!(
                        std::process::Command::new("mkfifo")
                            .arg(&output_for_worker)
                            .status()
                            .unwrap()
                            .success()
                    );
                },
            );
            sender.send(result).unwrap();
        });

        let result = match receiver.recv_timeout(Duration::from_millis(250)) {
            Ok(result) => result,
            Err(mpsc::RecvTimeoutError::Timeout) => {
                drop(fs::OpenOptions::new().write(true).open(&fifo).unwrap());
                let _ = receiver.recv_timeout(Duration::from_secs(1));
                worker.join().unwrap();
                panic!("prepared bundle loader blocked while opening a FIFO replacement");
            }
            Err(error) => panic!("prepared bundle loader disconnected: {error}"),
        };
        worker.join().unwrap();

        assert_eq!(result.unwrap_err().code(), "runtime_publication_invalid");
    }
}
