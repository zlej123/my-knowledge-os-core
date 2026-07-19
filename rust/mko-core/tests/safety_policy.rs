use std::{fs, path::Path};

#[test]
fn windows_acl_ffi_is_isolated_behind_a_safe_private_crate() {
    let core_root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let core_manifest = fs::read_to_string(core_root.join("Cargo.toml")).unwrap();
    let core_lib = fs::read_to_string(core_root.join("src/lib.rs")).unwrap();
    let helper_root = core_root.parent().unwrap().join("mko-windows-acl");

    assert!(
        core_lib.starts_with("#![forbid(unsafe_code)]"),
        "mko-core must retain its crate-wide unsafe-code prohibition"
    );
    assert_source_tree_has_no_unsafe_code(&core_root.join("src"));
    assert!(
        !core_manifest.contains("windows-sys"),
        "mko-core must not depend directly on raw Windows bindings"
    );
    assert!(
        core_manifest.contains("mko-windows-acl"),
        "mko-core must consume the safe Windows ACL helper"
    );

    let helper_manifest = fs::read_to_string(helper_root.join("Cargo.toml")).unwrap();
    let helper_lib = fs::read_to_string(helper_root.join("src/lib.rs")).unwrap();
    assert!(
        helper_manifest.contains("publish = false"),
        "the FFI helper must remain private to the workspace"
    );
    assert!(
        helper_manifest.contains("windows-sys"),
        "raw Windows bindings belong only in the FFI helper"
    );
    assert!(
        !helper_lib.contains("pub unsafe fn"),
        "the helper must not expose unsafe functions"
    );
    assert!(
        helper_lib.contains("pub struct FileIdentity")
            && helper_lib.contains("pub fn file_identity(file: &fs::File)"),
        "stable Windows file identity must be exposed through the safe helper"
    );
    assert!(
        helper_lib.contains("GetFileInformationByHandle")
            && helper_lib.contains("BY_HANDLE_FILE_INFORMATION"),
        "the safe helper must use stable by-handle Windows APIs"
    );

    for source in ["src/atomic.rs", "src/lock.rs"] {
        let source = fs::read_to_string(core_root.join(source)).unwrap();
        assert!(
            !source.contains("std::os::windows::fs::MetadataExt")
                && !source.contains("volume_serial_number()")
                && !source.contains("file_index()"),
            "mko-core must not use unstable Windows MetadataExt APIs"
        );
        assert!(
            source.contains("mko_windows_acl::file_identity"),
            "mko-core must route Windows identity through the safe helper"
        );
        assert!(
            source.contains("Windows has no supported POSIX-equivalent parent-directory fsync"),
            "the Windows parent-directory crash-durability limitation must remain explicit"
        );
    }
}

fn assert_source_tree_has_no_unsafe_code(directory: &Path) {
    for entry in fs::read_dir(directory).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            assert_source_tree_has_no_unsafe_code(&path);
        } else if path.extension().is_some_and(|extension| extension == "rs") {
            let source = fs::read_to_string(&path).unwrap();
            assert!(
                !source.contains("unsafe {")
                    && !source.contains("unsafe fn")
                    && !source.contains("unsafe impl")
                    && !source.contains("unsafe trait"),
                "unsafe Rust found in {}",
                path.display()
            );
        }
    }
}
