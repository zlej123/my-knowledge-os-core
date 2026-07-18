use std::{
    fs,
    path::{Path, PathBuf},
};

use chrono::{DateTime, Utc};
use chrono_tz::Asia::Seoul;
use unicode_normalization::UnicodeNormalization;

use crate::{
    atomic::{AtomicWriteResult, write_new, write_replace},
    clock::{Clock, SystemClock},
    config::load_capture_config,
    error::MkoError,
    fingerprint::{asset_id, fingerprint_open_file, validate_pdf_content},
    front_matter::{parse_markdown, render_markdown},
    lock::AssetLock,
    model::{AssetRecord, AssetStatus, Classification, LastError, LastSuccessfulStep, Provider},
    path_policy::{canonical_directory, provider_path, registry_directory, validate_ascii_slug},
    state::{previous_durable_state, transition_asset},
};

#[derive(Clone, Debug)]
pub struct CaptureRequest {
    repository_root: PathBuf,
    local_config_path: Option<PathBuf>,
    file: PathBuf,
    title: Option<String>,
    domains: Vec<String>,
    slug: Option<String>,
    captured_at: DateTime<Utc>,
}

impl CaptureRequest {
    pub fn new(repository_root: impl AsRef<Path>, file: impl AsRef<Path>) -> Self {
        Self {
            repository_root: repository_root.as_ref().to_path_buf(),
            local_config_path: None,
            file: file.as_ref().to_path_buf(),
            title: None,
            domains: Vec::new(),
            slug: None,
            captured_at: Utc::now(),
        }
    }

    pub fn with_local_config(mut self, path: impl AsRef<Path>) -> Self {
        self.local_config_path = Some(path.as_ref().to_path_buf());
        self
    }

    pub fn with_title(mut self, title: Option<String>) -> Self {
        self.title = title;
        self
    }

    pub fn with_domains(mut self, domains: Vec<String>) -> Self {
        self.domains = domains;
        self
    }

    pub fn with_slug(mut self, slug: Option<String>) -> Self {
        self.slug = slug;
        self
    }

    pub fn with_captured_at(mut self, captured_at: DateTime<Utc>) -> Self {
        self.captured_at = captured_at;
        self
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CaptureResult {
    pub result: String,
    pub asset_id: String,
    pub registry_path: String,
}

#[derive(Clone, Debug)]
pub struct AssetOperationRequest {
    repository_root: PathBuf,
    local_config_path: Option<PathBuf>,
    asset_id: String,
    clear_stale_lock: bool,
}

impl AssetOperationRequest {
    pub fn new(repository_root: impl AsRef<Path>, asset_id: impl Into<String>) -> Self {
        Self {
            repository_root: repository_root.as_ref().to_path_buf(),
            local_config_path: None,
            asset_id: asset_id.into(),
            clear_stale_lock: false,
        }
    }

    pub fn with_local_config(mut self, path: impl AsRef<Path>) -> Self {
        self.local_config_path = Some(path.as_ref().to_path_buf());
        self
    }

    pub fn with_clear_stale_lock(mut self, clear_stale_lock: bool) -> Self {
        self.clear_stale_lock = clear_stale_lock;
        self
    }
}

pub fn inspect_asset(request: AssetOperationRequest) -> Result<AssetRecord, MkoError> {
    inspect_asset_with_clock(request, &SystemClock)
}

pub fn inspect_asset_with_clock(
    request: AssetOperationRequest,
    clock: &dyn Clock,
) -> Result<AssetRecord, MkoError> {
    let config = load_capture_config(
        &request.repository_root,
        request.local_config_path.as_deref(),
    )?;
    let _lock = AssetLock::acquire(
        &config.repository_root,
        &request.asset_id,
        "mko asset inspect",
        clock,
        request.clear_stale_lock,
    )?;
    let (mut asset, body, path) = read_asset_document(&config.repository_root, &request.asset_id)?;
    if asset.asset_status == AssetStatus::Superseded {
        return Ok(asset);
    }
    let candidate = config.provider_root.join(&asset.provider.locator);
    if !candidate.exists() {
        if asset.asset_status != AssetStatus::Missing {
            transition_asset(&mut asset, AssetStatus::Missing, clock.now_utc())?;
            write_asset(&path, &asset, &body)?;
        }
        return Ok(asset);
    }
    let mut provider_file = provider_path(&config.provider_root, &candidate)?.file;
    let snapshot = fingerprint_open_file(&mut provider_file)?;
    if snapshot.fingerprint != asset.fingerprint {
        if asset.asset_status != AssetStatus::Changed {
            transition_asset(&mut asset, AssetStatus::Changed, clock.now_utc())?;
            write_asset(&path, &asset, &body)?;
        }
    } else if matches!(
        asset.asset_status,
        AssetStatus::Changed | AssetStatus::Missing
    ) {
        let previous = previous_durable_state(&asset).ok_or_else(|| {
            MkoError::new(
                "invalid_state_transition",
                "changed or missing asset has no durable recovery checkpoint",
            )
        })?;
        transition_asset(&mut asset, previous, clock.now_utc())?;
        write_asset(&path, &asset, &body)?;
    }
    Ok(asset)
}

pub fn accept_changed_asset(request: AssetOperationRequest) -> Result<AssetRecord, MkoError> {
    accept_changed_asset_with_clock(request, &SystemClock)
}

pub fn accept_changed_asset_with_clock(
    request: AssetOperationRequest,
    clock: &dyn Clock,
) -> Result<AssetRecord, MkoError> {
    accept_changed_asset_with_before_publish_and_clock(request, clock, || {})
}

#[cfg(test)]
fn accept_changed_asset_with_before_publish<F>(
    request: AssetOperationRequest,
    before_publish: F,
) -> Result<AssetRecord, MkoError>
where
    F: FnOnce(),
{
    accept_changed_asset_with_before_publish_and_clock(request, &SystemClock, before_publish)
}

fn accept_changed_asset_with_before_publish_and_clock<F>(
    request: AssetOperationRequest,
    clock: &dyn Clock,
    before_publish: F,
) -> Result<AssetRecord, MkoError>
where
    F: FnOnce(),
{
    let config = load_capture_config(
        &request.repository_root,
        request.local_config_path.as_deref(),
    )?;
    let _lock = AssetLock::acquire(
        &config.repository_root,
        &request.asset_id,
        "mko asset accept-change",
        clock,
        request.clear_stale_lock,
    )?;
    let (mut old_asset, old_body, old_path) =
        read_asset_document(&config.repository_root, &request.asset_id)?;
    if old_asset.asset_status != AssetStatus::Changed {
        return Err(MkoError::new(
            "invalid_state_transition",
            "accept-change requires an asset in the changed state",
        ));
    }
    let candidate = config.provider_root.join(&old_asset.provider.locator);
    let mut provider_file = provider_path(&config.provider_root, &candidate)?.file;
    validate_pdf_content(&mut provider_file)?;
    let before = fingerprint_open_file(&mut provider_file)?;
    if before.fingerprint == old_asset.fingerprint {
        return Err(MkoError::new(
            "change_not_detected",
            "provider content matches the original asset fingerprint",
        ));
    }
    let new_id = asset_id(&before.fingerprint)?;
    let now = clock.now_utc();
    let new_asset = AssetRecord {
        id: new_id.clone(),
        record_type: "asset".into(),
        schema_version: 1,
        scope: "personal".into(),
        title: old_asset.title.clone(),
        classification: old_asset.classification.clone(),
        asset_class: old_asset.asset_class.clone(),
        media_type: old_asset.media_type.clone(),
        provider: old_asset.provider.clone(),
        size_bytes: before.size_bytes,
        modified_at: DateTime::<Utc>::from(before.modified_at.into_std()),
        fingerprint: before.fingerprint.clone(),
        asset_status: AssetStatus::Registered,
        durable_state_history: Vec::new(),
        supersedes: Some(old_asset.id.clone()),
        last_successful_step: LastSuccessfulStep::Registered,
        last_error: LastError {
            code: None,
            retryable: false,
        },
        created_at: now,
        updated_at: now,
    };
    let registry = registry_directory(&config.repository_root)?;
    let new_path = registry.join(format!("{new_id}.md"));
    let new_body = registry_body(now, &new_asset.provider.locator);
    let document = render_markdown(&new_asset, &new_body)?;
    before_publish();
    let after = fingerprint_open_file(&mut provider_file)?;
    if before != after {
        return Err(MkoError::new(
            "fingerprint_changed",
            "file changed during accept-change; no successor registry record was published",
        ));
    }
    match write_new(&new_path, document.as_bytes(), |existing| {
        validate_accepted_successor(existing, &new_asset)
    })? {
        AtomicWriteResult::Created | AtomicWriteResult::Existing => {}
    }

    transition_asset(&mut old_asset, AssetStatus::Superseded, now)?;
    write_asset(&old_path, &old_asset, &old_body)?;
    crate::source::mark_sources_stale_for_asset(&config.repository_root, &old_asset.id, now)?;
    Ok(new_asset)
}

pub fn repair_lineage(request: AssetOperationRequest) -> Result<(), MkoError> {
    repair_lineage_with_clock(request, &SystemClock)
}

pub fn repair_lineage_with_clock(
    request: AssetOperationRequest,
    clock: &dyn Clock,
) -> Result<(), MkoError> {
    let config = load_capture_config(
        &request.repository_root,
        request.local_config_path.as_deref(),
    )?;
    let _lock = AssetLock::acquire(
        &config.repository_root,
        &request.asset_id,
        "mko asset repair-lineage",
        clock,
        request.clear_stale_lock,
    )?;
    let (mut old_asset, body, path) =
        read_asset_document(&config.repository_root, &request.asset_id)?;
    if old_asset.asset_status == AssetStatus::Superseded {
        return Ok(());
    }
    if old_asset.asset_status != AssetStatus::Changed {
        return Err(MkoError::new(
            "invalid_state_transition",
            "repair-lineage requires an asset in the changed state",
        ));
    }
    if !has_successor(&config.repository_root, &old_asset.id)? {
        return Err(MkoError::new(
            "relation_missing",
            "no authoritative successor registry record supersedes this asset",
        ));
    }
    transition_asset(&mut old_asset, AssetStatus::Superseded, clock.now_utc())?;
    write_asset(&path, &old_asset, &body)
}

pub fn read_asset(repository_root: &Path, asset_id: &str) -> Result<AssetRecord, MkoError> {
    let repository_root = canonical_directory(repository_root, "repository_root_invalid")?;
    read_asset_document(&repository_root, asset_id).map(|(asset, _, _)| asset)
}

pub fn lineage_repair_needed(repository_root: &Path) -> Result<Vec<String>, MkoError> {
    let repository_root = canonical_directory(repository_root, "repository_root_invalid")?;
    let registry = registry_directory(&repository_root)?;
    let assets = read_registry_assets(&registry)?;
    let mut affected = assets
        .iter()
        .filter(|asset| asset.asset_status == AssetStatus::Changed)
        .filter(|asset| {
            assets
                .iter()
                .any(|successor| successor.supersedes.as_deref() == Some(&asset.id))
        })
        .map(|asset| asset.id.clone())
        .collect::<Vec<_>>();
    affected.extend(crate::source::source_state_mismatch_asset_ids(
        &repository_root,
    )?);
    affected.sort();
    affected.dedup();
    Ok(affected)
}

pub fn capture_asset(request: CaptureRequest) -> Result<CaptureResult, MkoError> {
    capture_asset_with_before_verify(request, || {})
}

fn capture_asset_with_before_verify<F>(
    request: CaptureRequest,
    before_verify: F,
) -> Result<CaptureResult, MkoError>
where
    F: FnOnce(),
{
    if let Some(slug) = &request.slug {
        validate_ascii_slug(slug)?;
    }
    if request
        .domains
        .iter()
        .any(|domain| domain.trim().is_empty())
    {
        return Err(MkoError::new(
            "invalid_domain",
            "domain values must not be empty",
        ));
    }

    let config = load_capture_config(
        &request.repository_root,
        request.local_config_path.as_deref(),
    )?;
    let provider_path = provider_path(&config.provider_root, &request.file)?;
    let extension_is_pdf = Path::new(&provider_path.logical_path)
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("pdf"));
    if !extension_is_pdf {
        return Err(MkoError::new(
            "unsupported_media_type",
            "v0.1 capture accepts PDF files only",
        ));
    }

    let mut file = provider_path.file;
    let before = fingerprint_open_file(&mut file)?;
    validate_pdf_content(&mut file)?;
    let id = asset_id(&before.fingerprint)?;
    let title = request
        .title
        .filter(|title| !title.trim().is_empty())
        .unwrap_or_else(|| title_from_logical_path(&provider_path.logical_path, &id));
    let record = AssetRecord {
        id: id.clone(),
        record_type: "asset".into(),
        schema_version: 1,
        scope: "personal".into(),
        title: title.nfc().collect(),
        classification: Classification::Personal,
        asset_class: "document".into(),
        media_type: "application/pdf".into(),
        provider: Provider {
            r#type: config.provider_type,
            locator: provider_path.logical_path,
            revision: None,
        },
        size_bytes: before.size_bytes,
        modified_at: DateTime::<Utc>::from(before.modified_at.into_std()),
        fingerprint: before.fingerprint.clone(),
        asset_status: AssetStatus::Registered,
        durable_state_history: Vec::new(),
        supersedes: None,
        last_successful_step: LastSuccessfulStep::Registered,
        last_error: LastError {
            code: None,
            retryable: false,
        },
        created_at: request.captured_at,
        updated_at: request.captured_at,
    };
    let registry_path = format!("assets/registry/{id}.md");
    let registry_directory = registry_directory(&config.repository_root)?;
    let destination = registry_directory.join(format!("{id}.md"));
    let body = registry_body(request.captured_at, &record.provider.locator);
    let document = render_markdown(&record, &body)?;
    before_verify();
    let after = fingerprint_open_file(&mut file)?;
    if before != after {
        return Err(MkoError::new(
            "fingerprint_changed",
            "file changed during capture; no registry record was published",
        ));
    }
    match write_new(&destination, document.as_bytes(), |existing| {
        validate_existing(existing, &id, &record.fingerprint.value)
    })? {
        AtomicWriteResult::Created => Ok(CaptureResult {
            result: "created".into(),
            asset_id: id,
            registry_path,
        }),
        AtomicWriteResult::Existing => Ok(CaptureResult {
            result: "existing".into(),
            asset_id: id,
            registry_path,
        }),
    }
}

fn validate_existing(
    path: &Path,
    expected_id: &str,
    expected_fingerprint: &str,
) -> Result<(), MkoError> {
    let input = fs::read_to_string(path)
        .map_err(|error| MkoError::new("registry_unreadable", error.to_string()))?;
    let record: AssetRecord = parse_markdown(&input)?.metadata;
    if record.id != expected_id
        || record.fingerprint.method != "sha256"
        || record.fingerprint.value != expected_fingerprint
    {
        return Err(MkoError::new(
            "registry_conflict",
            "deterministic registry path contains a different asset",
        ));
    }
    Ok(())
}

pub(crate) fn read_asset_document(
    repository_root: &Path,
    asset_id: &str,
) -> Result<(AssetRecord, String, PathBuf), MkoError> {
    validate_asset_id(asset_id)?;
    let registry = registry_directory(repository_root)?;
    let path = registry.join(format!("{asset_id}.md"));
    let input = fs::read_to_string(&path).map_err(|error| {
        let code = if error.kind() == std::io::ErrorKind::NotFound {
            "registry_not_found"
        } else {
            "registry_unreadable"
        };
        MkoError::new(code, error.to_string())
    })?;
    let document = parse_markdown::<AssetRecord>(&input)?;
    if document.metadata.id != asset_id {
        return Err(MkoError::new(
            "registry_conflict",
            "registry filename and asset ID disagree",
        ));
    }
    Ok((document.metadata, document.body, path))
}

pub(crate) fn write_asset(path: &Path, asset: &AssetRecord, body: &str) -> Result<(), MkoError> {
    let document = render_markdown(asset, body)?;
    write_replace(path, document.as_bytes())
}

pub(crate) fn mark_asset_extracted(repository_root: &Path, asset_id: &str) -> Result<(), MkoError> {
    let (mut asset, body, path) = read_asset_document(repository_root, asset_id)?;
    if asset.asset_status == AssetStatus::Extracted {
        return Ok(());
    }
    transition_asset(&mut asset, AssetStatus::Extracted, Utc::now())?;
    write_asset(&path, &asset, &body)
}

pub(crate) fn mark_asset_review_pending_with_clock(
    repository_root: &Path,
    asset_id: &str,
    clock: &dyn Clock,
) -> Result<(), MkoError> {
    let (mut asset, body, path) = read_asset_document(repository_root, asset_id)?;
    if asset.asset_status == AssetStatus::ReviewPending {
        return Ok(());
    }
    if asset.asset_status != AssetStatus::Extracted {
        return Err(MkoError::new(
            "invalid_state_transition",
            "only an extracted asset can become review_pending",
        ));
    }
    transition_asset(&mut asset, AssetStatus::ReviewPending, clock.now_utc())?;
    write_asset(&path, &asset, &body)
}

pub(crate) fn mark_asset_processed_with_clock(
    repository_root: &Path,
    asset_id: &str,
    clock: &dyn Clock,
) -> Result<(), MkoError> {
    let (mut asset, body, path) = read_asset_document(repository_root, asset_id)?;
    if asset.asset_status == AssetStatus::Processed {
        return Ok(());
    }
    if asset.asset_status != AssetStatus::ReviewPending {
        return Err(MkoError::new(
            "invalid_state_transition",
            "only a review_pending asset can become processed",
        ));
    }
    transition_asset(&mut asset, AssetStatus::Processed, clock.now_utc())?;
    write_asset(&path, &asset, &body)
}

fn validate_asset_id(asset_id: &str) -> Result<(), MkoError> {
    let hash = asset_id.strip_prefix("personal-asset-");
    if hash.is_none_or(|hash| {
        hash.len() != 64
            || !hash
                .bytes()
                .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
    }) {
        return Err(MkoError::new(
            "asset_id_invalid",
            "asset ID must be a content-addressed asset ID",
        ));
    }
    Ok(())
}

fn validate_accepted_successor(path: &Path, expected: &AssetRecord) -> Result<(), MkoError> {
    let input = fs::read_to_string(path)
        .map_err(|error| MkoError::new("registry_unreadable", error.to_string()))?;
    let existing: AssetRecord = parse_markdown(&input)?.metadata;
    if existing.id != expected.id
        || existing.fingerprint != expected.fingerprint
        || existing.supersedes != expected.supersedes
    {
        return Err(MkoError::new(
            "registry_conflict",
            "deterministic successor registry path contains a different asset lineage",
        ));
    }
    Ok(())
}

fn has_successor(repository_root: &Path, old_asset_id: &str) -> Result<bool, MkoError> {
    let registry = registry_directory(repository_root)?;
    Ok(read_registry_assets(&registry)?
        .iter()
        .any(|asset| asset.supersedes.as_deref() == Some(old_asset_id)))
}

fn read_registry_assets(registry: &Path) -> Result<Vec<AssetRecord>, MkoError> {
    let mut assets = Vec::new();
    for entry in fs::read_dir(registry)
        .map_err(|error| MkoError::new("registry_unreadable", error.to_string()))?
    {
        let entry =
            entry.map_err(|error| MkoError::new("registry_unreadable", error.to_string()))?;
        let path = entry.path();
        if path.extension().and_then(|extension| extension.to_str()) != Some("md") {
            continue;
        }
        let input = fs::read_to_string(&path)
            .map_err(|error| MkoError::new("registry_unreadable", error.to_string()))?;
        let asset: AssetRecord = parse_markdown(&input)?.metadata;
        assets.push(asset);
    }
    Ok(assets)
}

fn title_from_logical_path(path: &str, id: &str) -> String {
    Path::new(path)
        .file_stem()
        .and_then(|stem| stem.to_str())
        .filter(|title| !title.trim().is_empty())
        .map(|title| title.nfc().collect())
        .unwrap_or_else(|| format!("document-{}", &id["personal-asset-".len()..][..12]))
}

fn registry_body(captured_at: DateTime<Utc>, locator: &str) -> String {
    let date = captured_at.with_timezone(&Seoul).format("%Y-%m-%d");
    format!("# Asset Registry\n\nCaptured: {date}\n\nProvider locator: `{locator}`\n")
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        sync::atomic::{AtomicU64, Ordering},
    };

    use chrono::{DateTime, Utc};

    use super::{
        AssetOperationRequest, CaptureRequest, accept_changed_asset_with_before_publish,
        capture_asset, capture_asset_with_before_verify, inspect_asset,
    };

    static NEXT_TEST_ENV: AtomicU64 = AtomicU64::new(0);

    #[test]
    fn mutation_before_final_fingerprint_is_not_published() {
        let unique = NEXT_TEST_ENV.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!(
            "mko-registry-mutation-test-{}-{unique}",
            std::process::id()
        ));
        let repository = root.join("repository");
        let provider = root.join("provider");
        let local_config = root.join("local-config.yaml");
        fs::create_dir_all(&repository).unwrap();
        fs::create_dir_all(&provider).unwrap();
        fs::write(
            repository.join("knowledge-os.yaml"),
            "system: my-knowledge-os\nscope: personal\ncore_version: 0.1.0\nschema_version: 1\nprovider:\n  name: personal_google_drive\n  type: google-drive-stream\n  root_env: MKO_PERSONAL_PROVIDER_ROOT\n",
        )
        .unwrap();
        fs::write(
            &local_config,
            format!("provider_root: {}\n", provider.display()),
        )
        .unwrap();
        let pdf = provider.join("paper.pdf");
        fs::write(&pdf, b"%PDF-1.7\nfirst version").unwrap();
        let timestamp = DateTime::parse_from_rfc3339("2026-07-18T00:00:00Z")
            .unwrap()
            .with_timezone(&Utc);

        let error = capture_asset_with_before_verify(
            CaptureRequest::new(&repository, &pdf)
                .with_local_config(&local_config)
                .with_captured_at(timestamp),
            || fs::write(&pdf, b"%PDF-1.7\nsecond version").unwrap(),
        )
        .unwrap_err();

        assert_eq!(error.code(), "fingerprint_changed");
        assert!(
            fs::read_dir(repository.join("assets/registry"))
                .map(|entries| entries.count())
                .unwrap_or_default()
                == 0
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn accept_change_discards_a_successor_when_the_retained_provider_file_changes() {
        let unique = NEXT_TEST_ENV.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!(
            "mko-accept-mutation-test-{}-{unique}",
            std::process::id()
        ));
        let repository = root.join("repository");
        let provider = root.join("provider");
        let local_config = root.join("local-config.yaml");
        fs::create_dir_all(&repository).unwrap();
        fs::create_dir_all(&provider).unwrap();
        fs::write(
            repository.join("knowledge-os.yaml"),
            "system: my-knowledge-os\nscope: personal\ncore_version: 0.1.0\nschema_version: 1\nprovider:\n  name: personal_google_drive\n  type: google-drive-stream\n  root_env: MKO_PERSONAL_PROVIDER_ROOT\n",
        )
        .unwrap();
        fs::write(
            &local_config,
            format!("provider_root: {}\n", provider.display()),
        )
        .unwrap();
        let pdf = provider.join("paper.pdf");
        fs::write(&pdf, b"%PDF-1.7\noriginal").unwrap();
        let captured =
            capture_asset(CaptureRequest::new(&repository, &pdf).with_local_config(&local_config))
                .unwrap();
        fs::write(&pdf, b"%PDF-1.7\nreplacement").unwrap();
        let request = AssetOperationRequest::new(&repository, &captured.asset_id)
            .with_local_config(&local_config);
        inspect_asset(request.clone()).unwrap();

        let error = accept_changed_asset_with_before_publish(request, || {
            fs::write(&pdf, b"%PDF-1.7\nchanged during acceptance").unwrap();
        })
        .unwrap_err();

        assert_eq!(error.code(), "fingerprint_changed");
        assert_eq!(
            fs::read_dir(repository.join("assets/registry"))
                .unwrap()
                .count(),
            1
        );
        let _ = fs::remove_dir_all(root);
    }
}
