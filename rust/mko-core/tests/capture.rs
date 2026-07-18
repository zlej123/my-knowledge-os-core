use std::{
    fs,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

use chrono::{DateTime, Utc};
use mko_core::{
    fingerprint::asset_id,
    model::Fingerprint,
    registry::{CaptureRequest, CaptureResult, capture_asset},
};

static NEXT_TEST_ENV: AtomicU64 = AtomicU64::new(0);

struct TestEnv {
    root: PathBuf,
    repository: PathBuf,
    provider: PathBuf,
    local_config: PathBuf,
}

impl TestEnv {
    fn new() -> Self {
        let unique = NEXT_TEST_ENV.fetch_add(1, Ordering::Relaxed);
        let root =
            std::env::temp_dir().join(format!("mko-capture-test-{}-{unique}", std::process::id()));
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

        Self {
            root,
            repository,
            provider,
            local_config,
        }
    }

    fn write_provider_file(&self, relative: &str, bytes: &[u8]) -> PathBuf {
        let path = self.provider.join(relative);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, bytes).unwrap();
        path
    }

    fn write_outside_file(&self, relative: &str, bytes: &[u8]) -> PathBuf {
        let path = self.root.join("outside").join(relative);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, bytes).unwrap();
        path
    }

    fn create_sparse_provider_file(&self, relative: &str, bytes: u64) -> PathBuf {
        let path = self.provider.join(relative);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::File::create(&path).unwrap().set_len(bytes).unwrap();
        path
    }

    fn capture(&self, file: &Path) -> Result<CaptureResult, mko_core::error::MkoError> {
        capture_asset(
            CaptureRequest::new(&self.repository, file)
                .with_local_config(&self.local_config)
                .with_captured_at(fixed_time()),
        )
    }

    fn registry_files(&self) -> Vec<PathBuf> {
        let registry = self.repository.join("assets/registry");
        fs::read_dir(registry)
            .map(|entries| entries.map(|entry| entry.unwrap().path()).collect())
            .unwrap_or_default()
    }
}

impl Drop for TestEnv {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn fixed_time() -> DateTime<Utc> {
    DateTime::parse_from_rfc3339("2026-07-18T00:00:00Z")
        .unwrap()
        .with_timezone(&Utc)
}

#[test]
fn same_bytes_produce_same_id_across_paths() {
    let env = TestEnv::new();
    let a = env.write_provider_file("a/paper.pdf", b"%PDF-fixture");
    let b = env.write_provider_file("b/paper-copy.pdf", b"%PDF-fixture");

    let first = env.capture(&a).unwrap();
    let before = fs::read(env.repository.join(&first.registry_path)).unwrap();
    let second = env.capture(&b).unwrap();
    let after = fs::read(env.repository.join(&second.registry_path)).unwrap();

    assert_eq!(first.asset_id, second.asset_id);
    assert_eq!(second.result, "existing");
    assert_eq!(before, after);
    assert_eq!(env.registry_files().len(), 1);
}

#[test]
fn id_rejects_non_hex_fingerprints() {
    let error = asset_id(&Fingerprint {
        method: "sha256".into(),
        value: format!("sha256:{}", "g".repeat(64)),
    })
    .unwrap_err();

    assert_eq!(error.code(), "fingerprint_invalid");
}

#[test]
fn rejects_paths_outside_provider_root() {
    let env = TestEnv::new();
    let outside = env.write_outside_file("paper.pdf", b"%PDF-fixture");

    let error = env.capture(&outside).unwrap_err();

    assert_eq!(error.code(), "outside_allowed_root");
    assert!(env.registry_files().is_empty());
}

#[test]
fn rejects_files_larger_than_fifty_mib() {
    let env = TestEnv::new();
    let file = env.create_sparse_provider_file("large.pdf", 50 * 1024 * 1024 + 1);

    let error = env.capture(&file).unwrap_err();

    assert_eq!(error.code(), "file_too_large");
    assert!(error.message().contains("manual processing"));
}

#[test]
fn rejects_non_pdf_content_with_a_pdf_suffix() {
    let env = TestEnv::new();
    let file = env.write_provider_file("not-a-pdf.pdf", b"plain text disguised as a PDF");

    let error = env.capture(&file).unwrap_err();

    assert_eq!(error.code(), "invalid_pdf");
    assert!(env.registry_files().is_empty());
}

#[test]
fn symlinked_files_cannot_escape_provider_root() {
    let env = TestEnv::new();
    let outside = env.write_outside_file("paper.pdf", b"%PDF-fixture");
    let linked = env.provider.join("linked.pdf");

    #[cfg(unix)]
    std::os::unix::fs::symlink(outside, &linked).unwrap();

    #[cfg(unix)]
    assert_eq!(
        env.capture(&linked).unwrap_err().code(),
        "outside_allowed_root"
    );
}
