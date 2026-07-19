use std::{
    collections::{BTreeMap, HashMap},
    ffi::{OsStr, OsString},
    fs,
    path::{Path, PathBuf},
};

use mko_core::{
    context::{
        ContextSource, PlatformEnvironment, ResolveContextRequest, Scope, resolve_personal_context,
    },
    profile::{MachineProfileFile, PersonalProfile, ProfileStore},
};
use tempfile::TempDir;

const KNOWLEDGE_CONFIG: &str = "system: my-knowledge-os\nscope: personal\ncore_version: 0.1.0\nschema_version: 1\nprovider:\n  name: personal_google_drive\n  type: google-drive-stream\n  root_env: MKO_PERSONAL_PROVIDER_ROOT\n";

#[derive(Clone)]
struct FakePlatform {
    config_home: PathBuf,
    home: PathBuf,
    current_dir: PathBuf,
    environment: HashMap<OsString, OsString>,
}

impl PlatformEnvironment for FakePlatform {
    fn config_home(&self) -> Result<PathBuf, mko_core::error::MkoError> {
        Ok(self.config_home.clone())
    }

    fn home_dir(&self) -> Result<PathBuf, mko_core::error::MkoError> {
        Ok(self.home.clone())
    }

    fn current_dir(&self) -> Result<PathBuf, mko_core::error::MkoError> {
        Ok(self.current_dir.clone())
    }

    fn environment_value(&self, name: &OsStr) -> Option<OsString> {
        self.environment.get(name).cloned()
    }
}

struct Fixture {
    _root: TempDir,
    root: PathBuf,
    platform: FakePlatform,
}

impl Fixture {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let config_home = root.path().join("platform config");
        let home = root.path().join("home");
        let current_dir = root.path().join("outside");
        fs::create_dir_all(&config_home).unwrap();
        fs::create_dir_all(&home).unwrap();
        fs::create_dir_all(&current_dir).unwrap();
        Self {
            root: root.path().to_path_buf(),
            _root: root,
            platform: FakePlatform {
                config_home,
                home,
                current_dir,
                environment: HashMap::new(),
            },
        }
    }

    fn repository(&self, name: &str) -> PathBuf {
        let repository = self.root.join(name);
        fs::create_dir_all(&repository).unwrap();
        fs::write(repository.join("knowledge-os.yaml"), KNOWLEDGE_CONFIG).unwrap();
        repository
    }

    fn provider(&self, name: &str) -> PathBuf {
        let provider = self.root.join(name);
        fs::create_dir_all(&provider).unwrap();
        provider
    }

    fn profile(&self, repository_root: &Path, provider_root: &Path) -> MachineProfileFile {
        MachineProfileFile {
            schema_version: 1,
            default_profile: "personal".into(),
            profiles: BTreeMap::from([(
                "personal".into(),
                PersonalProfile {
                    repository_root: repository_root.to_path_buf(),
                    provider_root: provider_root.to_path_buf(),
                    scope: Scope::Personal,
                },
            )]),
        }
    }

    fn store(&self) -> ProfileStore {
        ProfileStore::from_platform(&self.platform).unwrap()
    }
}

#[test]
fn profile_store_round_trip_is_owner_private_and_rejects_invalid_replacement() {
    let fixture = Fixture::new();
    let repository = fixture.repository("개인 지식 저장소");
    let provider = fixture.provider("Google Drive/내 드라이브");
    let store = fixture.store();
    let profile = fixture.profile(&repository, &provider);

    store.write(&profile).unwrap();

    assert_eq!(store.read().unwrap(), Some(profile));
    let before = fs::read(store.path()).unwrap();
    let invalid = MachineProfileFile {
        schema_version: 1,
        default_profile: "missing".into(),
        profiles: BTreeMap::new(),
    };
    assert_eq!(store.write(&invalid).unwrap_err().code(), "profile_invalid");
    assert_eq!(fs::read(store.path()).unwrap(), before);
    assert_eq!(
        fs::read_dir(store.path().parent().unwrap())
            .unwrap()
            .filter_map(Result::ok)
            .filter(|entry| entry.file_name().to_string_lossy().contains(".tmp"))
            .count(),
        0
    );

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            fs::metadata(store.path()).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(
            fs::metadata(store.path().parent().unwrap())
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
    }
}

#[cfg(unix)]
#[test]
fn profile_store_rejects_a_symlinked_profile_file() {
    let fixture = Fixture::new();
    let repository = fixture.repository("repository");
    let provider = fixture.provider("provider");
    let store = fixture.store();
    fs::create_dir_all(store.path().parent().unwrap()).unwrap();
    let outside = fixture.root.join("outside-profile.yaml");
    fs::write(&outside, "do not replace").unwrap();
    std::os::unix::fs::symlink(&outside, store.path()).unwrap();

    let error = store
        .write(&fixture.profile(&repository, &provider))
        .unwrap_err();

    assert_eq!(error.code(), "profile_path_invalid");
    assert_eq!(fs::read_to_string(outside).unwrap(), "do not replace");
}

#[test]
fn explicit_context_wins_without_mutating_the_profile() {
    let fixture = Fixture::new();
    let explicit_repository = fixture.repository("explicit repository");
    let ancestor_repository = fixture.repository("ancestor repository");
    let profile_repository = fixture.repository("profile repository");
    let profile_provider = fixture.provider("profile provider");
    let explicit_provider = fixture.provider("explicit provider");
    fixture
        .store()
        .write(&fixture.profile(&profile_repository, &profile_provider))
        .unwrap();
    let before = fs::read(fixture.store().path()).unwrap();
    let current_dir = ancestor_repository.join("nested/working");
    fs::create_dir_all(&current_dir).unwrap();
    let mut platform = fixture.platform.clone();
    platform.current_dir = current_dir;
    platform.environment.insert(
        OsString::from("MKO_PERSONAL_PROVIDER_ROOT"),
        explicit_provider.as_os_str().to_owned(),
    );

    let result = resolve_personal_context(
        ResolveContextRequest::new().with_explicit_repository(&explicit_repository),
        &platform,
    )
    .unwrap();

    assert_eq!(result.source, ContextSource::Explicit);
    assert_eq!(result.scope, Scope::Personal);
    assert_eq!(
        result.repository_root,
        explicit_repository.canonicalize().unwrap()
    );
    assert_eq!(
        result.provider_root,
        explicit_provider.canonicalize().unwrap()
    );
    assert_eq!(fs::read(fixture.store().path()).unwrap(), before);
}

#[test]
fn ancestor_knowledge_base_wins_over_the_default_profile() {
    let fixture = Fixture::new();
    let ancestor_repository = fixture.repository("ancestor repository");
    let ancestor_provider = fixture.provider("ancestor provider");
    let profile_repository = fixture.repository("profile repository");
    let profile_provider = fixture.provider("profile provider");
    fixture
        .store()
        .write(&fixture.profile(&profile_repository, &profile_provider))
        .unwrap();
    let current_dir = ancestor_repository.join("one/two/three");
    fs::create_dir_all(&current_dir).unwrap();
    let mut platform = fixture.platform.clone();
    platform.current_dir = current_dir;
    platform.environment.insert(
        OsString::from("MKO_PERSONAL_PROVIDER_ROOT"),
        ancestor_provider.as_os_str().to_owned(),
    );

    let result = resolve_personal_context(ResolveContextRequest::new(), &platform).unwrap();

    assert_eq!(result.source, ContextSource::Ancestor);
    assert_eq!(
        result.repository_root,
        ancestor_repository.canonicalize().unwrap()
    );
    assert_eq!(
        result.provider_root,
        ancestor_provider.canonicalize().unwrap()
    );
}

#[test]
fn default_profile_is_used_outside_a_knowledge_base_with_cross_platform_paths() {
    let fixture = Fixture::new();
    let repository = fixture.repository("Windows fixture/C Drive/사용자 지식 저장소");
    let provider = fixture.provider("Windows fixture/G Drive/내 드라이브");
    fixture
        .store()
        .write(&fixture.profile(&repository, &provider))
        .unwrap();

    let result = resolve_personal_context(ResolveContextRequest::new(), &fixture.platform).unwrap();

    assert_eq!(result.source, ContextSource::Profile);
    assert_eq!(result.profile_name, "personal");
    assert_eq!(result.repository_root, repository.canonicalize().unwrap());
    assert_eq!(result.provider_root, provider.canonicalize().unwrap());
    assert_eq!(result.provider_type, "google-drive-stream");
}

#[test]
fn conflicting_explicit_and_profile_scope_fails() {
    let fixture = Fixture::new();
    let repository = fixture.repository("repository");
    let provider = fixture.provider("provider");
    fixture
        .store()
        .write(&fixture.profile(&repository, &provider))
        .unwrap();

    let error = resolve_personal_context(
        ResolveContextRequest::new()
            .with_explicit_repository(&repository)
            .with_explicit_scope("shared"),
        &fixture.platform,
    )
    .unwrap_err();

    assert_eq!(error.code(), "scope_conflict");
}

#[test]
fn legacy_personal_config_imports_without_deletion() {
    let fixture = Fixture::new();
    let repository = fixture.repository("repository");
    let provider = fixture.provider("legacy provider");
    let legacy = fixture.platform.home.join(".config/mko/personal.yaml");
    fs::create_dir_all(legacy.parent().unwrap()).unwrap();
    let legacy_bytes = format!("provider_root: {}\n", provider.display()).into_bytes();
    fs::write(&legacy, &legacy_bytes).unwrap();

    let imported = fixture
        .store()
        .import_legacy_personal(&repository, &fixture.platform)
        .unwrap();

    assert_eq!(fs::read(&legacy).unwrap(), legacy_bytes);
    assert_eq!(fixture.store().read().unwrap(), Some(imported.clone()));
    assert_eq!(imported.default_profile, "personal");
    assert_eq!(
        imported.profiles["personal"].provider_root,
        provider.canonicalize().unwrap()
    );
}
