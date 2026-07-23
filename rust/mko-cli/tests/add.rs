use std::{
    collections::BTreeMap,
    fs,
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
};

use assert_cmd::Command;
use lopdf::{
    Document, Object, Stream,
    content::{Content, Operation},
    dictionary,
};
use mko_core::{
    context::Scope,
    profile::{MachineProfileFile, PersonalProfile, ProfileStore},
};

static NEXT_ENV: AtomicU64 = AtomicU64::new(0);

#[test]
#[allow(deprecated)]
fn add_is_a_concise_positional_pdf_command() {
    let root = std::env::temp_dir().join(format!(
        "mko-cli-add-{}-{}",
        std::process::id(),
        NEXT_ENV.fetch_add(1, Ordering::Relaxed)
    ));
    fs::create_dir_all(&root).unwrap();
    let pdf = root.join("paper.pdf");
    write_pdf(&pdf, "test");

    Command::cargo_bin("mko")
        .unwrap()
        .args(["add", &pdf.display().to_string(), "--format", "json-v1"])
        .env("HOME", root.join("empty-home"))
        .current_dir(&root)
        .assert()
        .code(1)
        .stdout(predicates::str::contains("\"command\":\"add\""));
    let _ = fs::remove_dir_all(root);
}

#[test]
#[allow(deprecated)]
fn human_add_and_backup_flags_preserve_the_core_backup_policy() {
    let env = AddEnv::new();
    let outside = env.root.join("outside.pdf");
    write_pdf(&outside, "outside");
    let output = env
        .command(["add", &outside.display().to_string()])
        .assert()
        .success()
        .stderr("")
        .get_output()
        .stdout
        .clone();
    assert!(
        String::from_utf8(output)
            .unwrap()
            .starts_with("created personal-asset-")
    );

    let temporary = env.root.join("temporary.pdf");
    write_pdf(&temporary, "temporary");
    env.command([
        "add",
        &temporary.display().to_string(),
        "--temporary-source",
    ])
    .assert()
    .code(1)
    .stdout("")
    .stderr(predicates::str::contains("backup_confirmation_required"));
    env.command([
        "add",
        &temporary.display().to_string(),
        "--temporary-source",
        "--verified-backup",
    ])
    .assert()
    .success();

    let inbox_only = env.provider.join("inbox-only.pdf");
    write_pdf(&inbox_only, "inbox only");
    env.command(["add", &inbox_only.display().to_string()])
        .assert()
        .code(1)
        .stdout("")
        .stderr(predicates::str::contains("backup_confirmation_required"));
    env.command([
        "add",
        &inbox_only.display().to_string(),
        "--verified-backup",
    ])
    .assert()
    .success();
}

struct AddEnv {
    root: PathBuf,
    provider: PathBuf,
    home: PathBuf,
}

impl AddEnv {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "mko-cli-add-profile-{}-{}",
            std::process::id(),
            NEXT_ENV.fetch_add(1, Ordering::Relaxed)
        ));
        let repository = root.join("repository");
        let provider = root.join("provider");
        let home = root.join("home");
        fs::create_dir_all(&repository).unwrap();
        fs::create_dir_all(&provider).unwrap();
        let config_home = config_home(&home);
        fs::write(repository.join("knowledge-os.yaml"), knowledge_config()).unwrap();
        ProfileStore::at(config_home.join("mko/profiles.yaml"))
            .write(&MachineProfileFile {
                schema_version: 1,
                default_profile: "personal".into(),
                profiles: BTreeMap::from([(
                    "personal".into(),
                    PersonalProfile {
                        repository_root: repository,
                        provider_root: provider.clone(),
                        scope: Scope::Personal,
                    },
                )]),
            })
            .unwrap();
        Self {
            root,
            provider,
            home,
        }
    }

    #[allow(deprecated)]
    fn command<I, S>(&self, args: I) -> Command
    where
        I: IntoIterator<Item = S>,
        S: AsRef<std::ffi::OsStr>,
    {
        let mut command = Command::cargo_bin("mko").unwrap();
        command
            .args(args)
            .env("HOME", &self.home)
            .env("APPDATA", config_home(&self.home))
            .env("XDG_CONFIG_HOME", config_home(&self.home))
            .current_dir(&self.root);
        command
    }
}

impl Drop for AddEnv {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

#[cfg(windows)]
fn config_home(home: &std::path::Path) -> PathBuf {
    home.join("AppData/Roaming")
}

#[cfg(target_os = "macos")]
fn config_home(home: &std::path::Path) -> PathBuf {
    home.join("Library/Application Support")
}

#[cfg(not(any(windows, target_os = "macos")))]
fn config_home(home: &std::path::Path) -> PathBuf {
    home.join(".config")
}

fn knowledge_config() -> &'static str {
    "system: my-knowledge-os\nscope: personal\ncore_version: 0.1.0\nschema_version: 1\nprovider:\n  name: personal_google_drive\n  type: google-drive-stream\n  root_env: MKO_PERSONAL_PROVIDER_ROOT\n"
}

fn write_pdf(path: &std::path::Path, text: &str) {
    let mut document = Document::with_version("1.5");
    let pages = document.new_object_id();
    let font = document.add_object(
        dictionary! { "Type" => "Font", "Subtype" => "Type1", "BaseFont" => "Helvetica" },
    );
    let resources = document.add_object(dictionary! { "Font" => dictionary! { "F1" => font } });
    let content = Content {
        operations: vec![
            Operation::new("BT", vec![]),
            Operation::new("Tf", vec![Object::Name(b"F1".to_vec()), 12.into()]),
            Operation::new("Tj", vec![Object::string_literal(text.as_bytes())]),
            Operation::new("ET", vec![]),
        ],
    }
    .encode()
    .unwrap();
    let contents = document.add_object(Stream::new(dictionary! {}, content));
    let page = document.add_object(dictionary! { "Type" => "Page", "Parent" => pages, "Contents" => contents, "Resources" => resources, "MediaBox" => vec![0.into(), 0.into(), 612.into(), 792.into()] });
    document.objects.insert(
        pages,
        dictionary! { "Type" => "Pages", "Kids" => vec![Object::Reference(page)], "Count" => 1 }
            .into(),
    );
    let catalog = document.add_object(dictionary! { "Type" => "Catalog", "Pages" => pages });
    document.trailer.set("Root", catalog);
    document.renumber_objects();
    document.save(path).unwrap();
}
