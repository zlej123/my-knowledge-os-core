use std::{
    fs::{self, OpenOptions},
    io::{Seek, SeekFrom, Write},
    path::{Component, Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

use atomic_write_file::AtomicWriteFile;
use serde::{Deserialize, Serialize};

use crate::{
    CORE_VERSION,
    config::load_capture_config,
    error::MkoError,
    fingerprint::{fingerprint_open_file, validate_pdf_content},
    lock::AssetLock,
    model::{AssetStatus, Fingerprint},
    path_policy::provider_path,
    pdf::{
        EXTRACTOR_NAME, EXTRACTOR_VERSION, extract_pdf_pages_in_child, validate_extracted_pages,
        validate_pdf_page_limit,
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
    let output = runtime_output_path(&config.repository_root, &request.output)?;
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
    let snapshot = Snapshot::copy_from(&output, &request.asset_id, &mut provider)?;
    validate_pdf_page_limit(&snapshot.path)?;
    let pages = extractor(&snapshot.path)?;
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
        source_id: source_id(&asset.id)?,
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
    write_runtime(&output, &bytes)?;
    let published: PreparedSourceBundle = serde_json::from_slice(
        &fs::read(&output)
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
    Ok(format!("personal-source-{hash}"))
}

fn runtime_output_path(repository_root: &Path, requested: &Path) -> Result<PathBuf, MkoError> {
    let runtime = repository_root.join(".knowledge-os").join("runtime");
    fs::create_dir_all(&runtime)
        .map_err(|error| MkoError::new("runtime_write_failed", error.to_string()))?;
    let runtime = fs::canonicalize(&runtime)
        .map_err(|error| MkoError::new("runtime_write_failed", error.to_string()))?;
    let candidate = if requested.is_absolute() {
        requested.to_path_buf()
    } else {
        repository_root.join(requested)
    };
    if candidate
        .components()
        .any(|component| component == Component::ParentDir)
    {
        return Err(MkoError::new(
            "outside_allowed_root",
            "prepared output must not contain path traversal",
        ));
    }
    let parent = candidate
        .parent()
        .ok_or_else(|| MkoError::new("outside_allowed_root", "prepared output has no parent"))?;
    let parent = fs::canonicalize(parent).map_err(|error| {
        MkoError::new(
            "outside_allowed_root",
            format!("prepared output parent is unavailable: {error}"),
        )
    })?;
    if !parent.starts_with(&runtime) {
        return Err(MkoError::new(
            "outside_allowed_root",
            "prepared output must be under .knowledge-os/runtime",
        ));
    }
    let filename = candidate
        .file_name()
        .ok_or_else(|| MkoError::new("outside_allowed_root", "prepared output must name a file"))?;
    Ok(parent.join(filename))
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

struct Snapshot {
    path: PathBuf,
}

impl Snapshot {
    fn copy_from(
        output: &Path,
        asset_id: &str,
        provider: &mut cap_std::fs::File,
    ) -> Result<Self, MkoError> {
        let parent = output.parent().ok_or_else(|| {
            MkoError::new("runtime_write_failed", "prepared output has no parent")
        })?;
        let path = parent.join(format!(
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
        let result = std::io::copy(provider, &mut snapshot)
            .and_then(|_| snapshot.sync_all())
            .map_err(|error| MkoError::new("runtime_write_failed", error.to_string()));
        provider
            .seek(SeekFrom::Start(0))
            .map_err(|error| MkoError::new("file_unreadable", error.to_string()))?;
        if let Err(error) = result {
            let _ = fs::remove_file(&path);
            return Err(error);
        }
        Ok(Self { path })
    }
}

impl Drop for Snapshot {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}
