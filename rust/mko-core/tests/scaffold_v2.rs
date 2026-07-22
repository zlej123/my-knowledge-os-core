use std::{fs, path::Path};

use mko_core::{
    config_v2::{KnowledgeConfigV2, SCHEMA_VERSION_V2},
    scaffold_v2::{ScaffoldOutcomeV2, scaffold_personal_kb_v2},
};

#[test]
fn creates_an_idempotent_v2_personal_kb() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("personal-kb");

    assert_eq!(
        scaffold_personal_kb_v2(&root).unwrap(),
        ScaffoldOutcomeV2::Created
    );
    assert_eq!(
        scaffold_personal_kb_v2(&root).unwrap(),
        ScaffoldOutcomeV2::Existing
    );
    let config = KnowledgeConfigV2::read(&root).unwrap();
    assert_eq!(config.schema_version, SCHEMA_VERSION_V2);
    for relative in [
        "assets/registry",
        "sources",
        "knowledge",
        "reviews",
        "views/records",
        "recovery/manual-edits",
    ] {
        assert!(root.join(relative).is_dir(), "missing {relative}");
    }
}

#[test]
fn repairs_only_missing_owned_directories() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("personal-kb");
    scaffold_personal_kb_v2(&root).unwrap();
    fs::remove_dir(root.join("views/records")).unwrap();

    assert_eq!(
        scaffold_personal_kb_v2(&root).unwrap(),
        ScaffoldOutcomeV2::Repaired
    );
    assert!(root.join("views/records").is_dir());
}

#[test]
fn rejects_nonempty_unmanaged_and_old_schema_destinations() {
    let temp = tempfile::tempdir().unwrap();
    let nonempty = temp.path().join("nonempty");
    fs::create_dir(&nonempty).unwrap();
    fs::write(nonempty.join("note.md"), "mine").unwrap();
    assert_eq!(
        scaffold_personal_kb_v2(&nonempty).unwrap_err().code(),
        "kb_destination_not_empty"
    );

    let old = temp.path().join("old");
    fs::create_dir(&old).unwrap();
    fs::write(
        old.join("knowledge-os.yaml"),
        "system: my-knowledge-os\nscope: personal\ncore_version: 0.1.0\nschema_version: 1\nprovider:\n  name: personal\n  type: google-drive-stream\n  root_env: OLD\n",
    )
    .unwrap();
    assert_eq!(
        scaffold_personal_kb_v2(&old).unwrap_err().code(),
        "kb_config_invalid"
    );
}

#[cfg(unix)]
#[test]
fn rejects_symlinked_managed_paths() {
    use std::os::unix::fs::symlink;

    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("personal-kb");
    fs::create_dir(&root).unwrap();
    let config = KnowledgeConfigV2::personal_default().render().unwrap();
    fs::write(root.join("knowledge-os.yaml"), config).unwrap();
    let outside = temp.path().join("outside");
    fs::create_dir(&outside).unwrap();
    symlink(&outside, root.join("assets")).unwrap();

    assert_eq!(
        scaffold_personal_kb_v2(&root).unwrap_err().code(),
        "kb_path_invalid"
    );
    assert!(directory_is_empty(&outside));
}

fn directory_is_empty(path: &Path) -> bool {
    fs::read_dir(path).unwrap().next().is_none()
}
