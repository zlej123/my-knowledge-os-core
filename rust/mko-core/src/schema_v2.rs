use serde_json::Value;

use crate::error::MkoError;
use crate::json_v2::{SchemaDescriptorV2, SchemaListDataV2, SchemaShowDataV2};

struct EmbeddedSchemaV2 {
    name: &'static str,
    purpose: &'static str,
    schema: &'static str,
    example: &'static str,
}

// Embedded at build time so an installed CLI can serve its own contracts:
// the Skill must never depend on a repository checkout for schemas, and an
// embedded copy can never disagree with the binary that validates it. The
// examples are the same fixtures the contract tests validate against both
// the JSON Schemas and the Rust deserializers.
const EMBEDDED_SCHEMAS_V2: &[EmbeddedSchemaV2] = &[
    EmbeddedSchemaV2 {
        name: "source-response-v2",
        purpose: "semantic response an agent authors for `mko source write-draft --response`",
        schema: include_str!("../../../schemas/v2/source-response.schema.json"),
        example: include_str!("../../../tests/fixtures/json-v2/source-response.json"),
    },
    EmbeddedSchemaV2 {
        name: "knowledge-response-v2",
        purpose: "semantic response an agent authors for `mko knowledge write --response`",
        schema: include_str!("../../../schemas/v2/knowledge-response.schema.json"),
        example: include_str!("../../../tests/fixtures/json-v2/knowledge-response.json"),
    },
    EmbeddedSchemaV2 {
        name: "review-feedback-input-v2",
        purpose: "bounded decision input an agent passes to `mko review-feedback --input`",
        schema: include_str!("../../../schemas/v2/review-feedback-input.schema.json"),
        example: include_str!("../../../tests/fixtures/json-v2/review-feedback-input.json"),
    },
];

pub fn list_schemas_v2() -> SchemaListDataV2 {
    SchemaListDataV2 {
        schemas: EMBEDDED_SCHEMAS_V2
            .iter()
            .map(|entry| SchemaDescriptorV2 {
                name: entry.name.into(),
                purpose: entry.purpose.into(),
            })
            .collect(),
    }
}

pub fn show_schema_v2(name: &str) -> Result<SchemaShowDataV2, MkoError> {
    let entry = EMBEDDED_SCHEMAS_V2
        .iter()
        .find(|entry| entry.name == name)
        .ok_or_else(|| {
            let known = EMBEDDED_SCHEMAS_V2
                .iter()
                .map(|entry| entry.name)
                .collect::<Vec<_>>()
                .join(", ");
            MkoError::new(
                "schema_not_found",
                format!(
                    "no embedded contract is named {name}; this CLI provides: {known}. \
                     An unknown name usually means the Skill and CLI are out of sync"
                ),
            )
        })?;
    Ok(SchemaShowDataV2 {
        name: entry.name.into(),
        purpose: entry.purpose.into(),
        schema: parse_embedded(entry.name, "schema", entry.schema)?,
        example: parse_embedded(entry.name, "example", entry.example)?,
    })
}

fn parse_embedded(name: &str, part: &str, bytes: &str) -> Result<Value, MkoError> {
    serde_json::from_str(bytes).map_err(|error| {
        MkoError::new(
            "schema_embed_invalid",
            format!("the embedded {part} for {name} is not valid JSON: {error}"),
        )
    })
}

#[cfg(test)]
mod tests {
    use super::{EMBEDDED_SCHEMAS_V2, list_schemas_v2, show_schema_v2};

    #[test]
    fn every_embedded_schema_parses_and_lists_uniquely() {
        let list = list_schemas_v2();
        assert_eq!(list.schemas.len(), EMBEDDED_SCHEMAS_V2.len());
        let mut names = list
            .schemas
            .iter()
            .map(|schema| schema.name.as_str())
            .collect::<Vec<_>>();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), EMBEDDED_SCHEMAS_V2.len());
        for schema in &list.schemas {
            let shown = show_schema_v2(&schema.name).unwrap();
            assert!(shown.schema.is_object());
            assert!(shown.example.is_object());
        }
    }

    #[test]
    fn unknown_schema_names_report_the_known_contracts() {
        let error = show_schema_v2("source-response-v1").unwrap_err();
        assert_eq!(error.code(), "schema_not_found");
        assert!(error.message().contains("source-response-v2"));
        assert!(error.message().contains("review-feedback-input-v2"));
    }
}
