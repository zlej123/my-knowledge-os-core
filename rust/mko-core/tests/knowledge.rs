use mko_core::knowledge::{
    ConceptKind, normalize_and_validate_knowledge, parse_knowledge_response,
};

const VALID: &str = r#"{
  "synthesis": "A signals-and-systems text covering LTI systems and transforms.",
  "concepts": [
    {"name": "Convolution", "kind": "formula", "body": "x*h(t)=∫x(τ)h(t−τ)dτ", "tags": ["LTI"], "locator": "§4.2"},
    {"name": "Causal signal", "kind": "definition", "body": "x(t)=0 for t<0", "tags": [], "locator": null}
  ]
}"#;

#[test]
fn parses_and_validates_a_well_formed_response() {
    let mut r = parse_knowledge_response(VALID.as_bytes()).unwrap();
    normalize_and_validate_knowledge(&mut r).unwrap();
    assert!(!r.synthesis.is_empty());
    assert_eq!(r.concepts.len(), 2);
    assert_eq!(r.concepts[0].kind, ConceptKind::Formula);
}

#[test]
fn rejects_unknown_fields() {
    let bad = r#"{"synthesis":"x","concepts":[],"extra":1}"#;
    assert!(parse_knowledge_response(bad.as_bytes()).is_err());
}

#[test]
fn rejects_empty_synthesis() {
    let bad = r#"{"synthesis":"   ","concepts":[]}"#;
    let mut r = parse_knowledge_response(bad.as_bytes()).unwrap();
    assert_eq!(
        normalize_and_validate_knowledge(&mut r).unwrap_err().code(),
        "semantic_schema_invalid"
    );
}

#[test]
fn rejects_concept_with_empty_body_or_multiline_name() {
    for bad in [
        r#"{"synthesis":"x","concepts":[{"name":"A","kind":"concept","body":"  ","tags":[],"locator":null}]}"#,
        r#"{"synthesis":"x","concepts":[{"name":"A\nB","kind":"concept","body":"y","tags":[],"locator":null}]}"#,
    ] {
        let mut r = parse_knowledge_response(bad.as_bytes()).unwrap();
        assert!(normalize_and_validate_knowledge(&mut r).is_err());
    }
}

#[test]
fn rejects_invalid_kind() {
    let bad = r#"{"synthesis":"x","concepts":[{"name":"A","kind":"joke","body":"y","tags":[],"locator":null}]}"#;
    assert!(parse_knowledge_response(bad.as_bytes()).is_err());
}

#[test]
fn allows_empty_concepts() {
    let mut r = parse_knowledge_response(r#"{"synthesis":"x","concepts":[]}"#.as_bytes()).unwrap();
    normalize_and_validate_knowledge(&mut r).unwrap();
}
