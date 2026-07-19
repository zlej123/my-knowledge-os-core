use std::{
    fs,
    sync::atomic::{AtomicU64, Ordering},
};

use assert_cmd::Command;
use lopdf::{
    Document, Object, Stream,
    content::{Content, Operation},
    dictionary,
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
    write_pdf(&pdf);

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

fn write_pdf(path: &std::path::Path) {
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
            Operation::new("Tj", vec![Object::string_literal(b"test")]),
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
