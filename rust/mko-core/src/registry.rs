use std::{
    fs,
    path::{Path, PathBuf},
};

use chrono::{DateTime, Utc};
use chrono_tz::Asia::Seoul;
use unicode_normalization::UnicodeNormalization;

use crate::{
    atomic::{AtomicWriteResult, write_new},
    config::load_capture_config,
    error::MkoError,
    fingerprint::{asset_id, fingerprint_open_file, validate_pdf_content},
    front_matter::{parse_markdown, render_markdown},
    model::{AssetRecord, AssetStatus, Classification, LastError, LastSuccessfulStep, Provider},
    path_policy::{provider_path, registry_directory, validate_ascii_slug},
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
    let destination_exists = destination.exists();
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
    if destination_exists {
        return existing_result(&destination, &id, &record.fingerprint.value, registry_path);
    }
    match write_new(&destination, document.as_bytes())? {
        AtomicWriteResult::Created => Ok(CaptureResult {
            result: "created".into(),
            asset_id: id,
            registry_path,
        }),
        AtomicWriteResult::Existing => {
            existing_result(&destination, &id, &record.fingerprint.value, registry_path)
        }
    }
}

fn existing_result(
    path: &Path,
    expected_id: &str,
    expected_fingerprint: &str,
    registry_path: String,
) -> Result<CaptureResult, MkoError> {
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
    Ok(CaptureResult {
        result: "existing".into(),
        asset_id: expected_id.into(),
        registry_path,
    })
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

    use super::{CaptureRequest, capture_asset_with_before_verify};

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
}
