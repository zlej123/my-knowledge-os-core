use mko_core::{
    front_matter::{parse_markdown, render_markdown},
    model::SourceRecord,
    revision::calculate_source_revision,
    safe_yaml::validate_yaml_input,
};

mod support;
use support::fixture_source;

#[test]
fn rejects_aliases_before_deserialization() {
    let input = "---\na: &x [1]\nb: *x\n---\n# Body\n";
    let error = parse_markdown::<SourceRecord>(input).unwrap_err();
    assert_eq!(error.code(), "unsafe_yaml");
}

#[test]
fn revision_changes_when_source_metadata_changes() {
    let mut source = fixture_source();
    let before = calculate_source_revision(&source, "# Body\n").unwrap();
    source.source_metadata.doi = Some("10.1000/changed".into());
    let after = calculate_source_revision(&source, "# Body\n").unwrap();
    assert_ne!(before, after);
}

#[test]
fn rendering_round_trips_with_lf() {
    let source = fixture_source();
    let rendered = render_markdown(&source, "# Body\r\n").unwrap();
    assert!(!rendered.contains('\r'));
    let parsed: SourceRecord = parse_markdown(&rendered).unwrap().metadata;
    assert_eq!(parsed.id, source.id);
}

#[test]
fn rejects_a_second_yaml_document() {
    let source = fixture_source();
    let rendered = render_markdown(&source, "# Body\n").unwrap();
    let input = rendered.replacen("---\n# Body", "---\n---\nignored: document\n---\n# Body", 1);

    let error = parse_markdown::<SourceRecord>(&input).unwrap_err();
    assert_eq!(error.code(), "yaml_invalid");
}

#[test]
fn revision_ignores_tag_order_and_duplicates() {
    let mut source = fixture_source();
    let before = calculate_source_revision(&source, "# Body\n").unwrap();
    source.tags = vec!["fixture".into(), "alpha".into(), "fixture".into()];
    let with_new_tag = calculate_source_revision(&source, "# Body\n").unwrap();
    source.tags = vec!["alpha".into(), "fixture".into()];
    let reordered = calculate_source_revision(&source, "# Body\n").unwrap();

    assert_ne!(before, with_new_tag);
    assert_eq!(with_new_tag, reordered);
}

#[test]
fn rejects_inline_tags_before_deserialization() {
    let input = "---\ntitle: !<tag:yaml.org,2002:str> unsafe\n---\n# Body\n";
    let error = parse_markdown::<serde_json::Value>(input).unwrap_err();
    assert_eq!(error.code(), "unsafe_yaml");
}

#[test]
fn rejects_semantically_duplicate_quoted_keys() {
    let input = "---\n\"title\": first\ntitle: second\n---\n# Body\n";
    let error = parse_markdown::<serde_json::Value>(input).unwrap_err();
    assert_eq!(error.code(), "yaml_invalid");
}

#[test]
fn rejects_compact_sequences_deeper_than_the_limit() {
    let input = format!("root:\n  {} value\n", "- ".repeat(33));
    let error = validate_yaml_input(&input).unwrap_err();
    assert_eq!(error.code(), "unsafe_yaml");
}

#[test]
fn revision_normalizes_line_endings_in_metadata() {
    let mut source = fixture_source();
    source.title = "Fixture\r\nSource".into();
    source.source_metadata.authors = vec!["Fixture\r\nAuthor".into()];
    let crlf = calculate_source_revision(&source, "# Body\n").unwrap();

    source.title = "Fixture\nSource".into();
    source.source_metadata.authors = vec!["Fixture\nAuthor".into()];
    let lf = calculate_source_revision(&source, "# Body\n").unwrap();

    assert_eq!(crlf, lf);
}

#[test]
fn source_schema_limits_relations_to_one_asset() {
    let schema_path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../schemas/source-v1.schema.json"
    );
    let schema: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(schema_path).unwrap()).unwrap();

    assert_eq!(
        schema["properties"]["relations"]["properties"]["asset_ids"]["maxItems"],
        1
    );
}
