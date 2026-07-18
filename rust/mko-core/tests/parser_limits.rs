#[path = "support/pdf_fixture.rs"]
mod pdf_fixture;

use std::{fs, time::Duration};

use mko_core::{
    front_matter::parse_markdown,
    pdf::{EXTRACTION_TIMEOUT, MAX_EXTRACTED_TEXT_BYTES, extract_pdf_pages_from_bytes},
};
use serde_json::Value;

use pdf_fixture::write_pdf;

const ALIAS_FIXTURE: &str = "---\nroot: &root\n  value: fixture\nalias: *root\n---\nbody\n";
const DUPLICATE_KEY_FIXTURE: &str = "---\ntitle: first\ntitle: second\n---\nbody\n";

#[test]
fn generated_pdf_above_one_thousand_pages_has_exact_error_code() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("1,001-pages.pdf");
    let pages = (0..1_001)
        .map(|page| format!("Page {page}"))
        .collect::<Vec<_>>();
    write_pdf(&path, &pages);

    let error = extract_pdf_pages_from_bytes(&fs::read(path).unwrap()).unwrap_err();

    assert_eq!(error.code(), "page_limit_exceeded");
}

#[test]
fn generated_pdf_above_extracted_text_limit_has_exact_error_code() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("oversized-extracted-text.pdf");
    write_pdf(&path, &["x".repeat(MAX_EXTRACTED_TEXT_BYTES + 1)]);

    let error = extract_pdf_pages_from_bytes(&fs::read(path).unwrap()).unwrap_err();

    assert_eq!(error.code(), "extracted_text_too_large");
}

#[test]
fn extraction_timeout_is_pinned_to_the_acceptance_bound() {
    assert_eq!(EXTRACTION_TIMEOUT, Duration::from_secs(120));
}

#[test]
fn yaml_alias_fixture_has_exact_error_code() {
    let error = parse_markdown::<Value>(ALIAS_FIXTURE).unwrap_err();

    assert_eq!(error.code(), "unsafe_yaml");
}

#[test]
fn excessive_yaml_nesting_fixture_has_exact_error_code() {
    let fixture = format!(
        "---\nroot: {}value{}\n---\nbody\n",
        "[".repeat(33),
        "]".repeat(33)
    );

    let error = parse_markdown::<Value>(&fixture).unwrap_err();

    assert_eq!(error.code(), "unsafe_yaml");
}

#[test]
fn duplicate_yaml_key_fixture_has_exact_error_code() {
    let error = parse_markdown::<Value>(DUPLICATE_KEY_FIXTURE).unwrap_err();

    assert_eq!(error.code(), "yaml_invalid");
}

#[test]
fn yaml_document_above_256_kib_has_exact_error_code() {
    let fixture = format!("---\nvalue: {}\n---\nbody\n", "x".repeat(256 * 1024));

    let error = parse_markdown::<Value>(&fixture).unwrap_err();

    assert_eq!(error.code(), "unsafe_yaml");
}
