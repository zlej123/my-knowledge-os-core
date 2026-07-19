use mko_core::json_v1::{JsonV1Failure, JsonV1Success};
use serde_json::{Value, json};

const GOLDENS: &[(&str, &[u8], bool)] = &[
    (
        "add-created.json",
        include_bytes!("../../../tests/fixtures/json-v1/add-created.json"),
        true,
    ),
    (
        "add-error.json",
        include_bytes!("../../../tests/fixtures/json-v1/add-error.json"),
        false,
    ),
    (
        "add-inbox-mixed.json",
        include_bytes!("../../../tests/fixtures/json-v1/add-inbox-mixed.json"),
        true,
    ),
    (
        "add-inbox-error.json",
        include_bytes!("../../../tests/fixtures/json-v1/add-inbox-error.json"),
        false,
    ),
    (
        "source-prepare.json",
        include_bytes!("../../../tests/fixtures/json-v1/source-prepare.json"),
        true,
    ),
    (
        "source-prepare-error.json",
        include_bytes!("../../../tests/fixtures/json-v1/source-prepare-error.json"),
        false,
    ),
    (
        "source-write-draft.json",
        include_bytes!("../../../tests/fixtures/json-v1/source-write-draft.json"),
        true,
    ),
    (
        "source-write-draft-error.json",
        include_bytes!("../../../tests/fixtures/json-v1/source-write-draft-error.json"),
        false,
    ),
    (
        "check-valid.json",
        include_bytes!("../../../tests/fixtures/json-v1/check-valid.json"),
        true,
    ),
    (
        "check-error.json",
        include_bytes!("../../../tests/fixtures/json-v1/check-error.json"),
        false,
    ),
    (
        "doctor-healthy.json",
        include_bytes!("../../../tests/fixtures/json-v1/doctor-healthy.json"),
        true,
    ),
    (
        "doctor-error.json",
        include_bytes!("../../../tests/fixtures/json-v1/doctor-error.json"),
        false,
    ),
    (
        "inbox-success.json",
        include_bytes!("../../../tests/fixtures/json-v1/inbox-success.json"),
        true,
    ),
    (
        "inbox-incomplete.json",
        include_bytes!("../../../tests/fixtures/json-v1/inbox-incomplete.json"),
        true,
    ),
    (
        "inbox-error.json",
        include_bytes!("../../../tests/fixtures/json-v1/inbox-error.json"),
        false,
    ),
    (
        "status-success.json",
        include_bytes!("../../../tests/fixtures/json-v1/status-success.json"),
        true,
    ),
    (
        "status-blocked.json",
        include_bytes!("../../../tests/fixtures/json-v1/status-blocked.json"),
        true,
    ),
    (
        "status-error.json",
        include_bytes!("../../../tests/fixtures/json-v1/status-error.json"),
        false,
    ),
];

fn machine_schema() -> Value {
    serde_json::from_str(include_str!(
        "../../../schemas/machine-output-v1.schema.json"
    ))
    .expect("machine output schema is JSON")
}

fn assert_rejected_by_schema_and_rust(value: &Value) {
    let validator = jsonschema::validator_for(&machine_schema()).expect("schema compiles");
    assert!(!validator.is_valid(value), "schema accepted {value}");
    assert!(
        serde_json::from_value::<JsonV1Success>(value.clone()).is_err(),
        "success model accepted {value}"
    );
    assert!(
        serde_json::from_value::<JsonV1Failure>(value.clone()).is_err(),
        "failure model accepted {value}"
    );
}

#[test]
fn add_created_golden_round_trips_exactly() {
    let bytes = include_bytes!("../../../tests/fixtures/json-v1/add-created.json");
    let parsed: JsonV1Success = serde_json::from_slice(bytes).unwrap();
    assert!(matches!(
        parsed,
        JsonV1Success::Add {
            schema_version: 1,
            ..
        }
    ));
    assert_eq!(
        serde_json::to_value(parsed).unwrap(),
        serde_json::from_slice::<Value>(bytes).unwrap()
    );
}

#[test]
fn all_goldens_validate_and_typed_round_trip_exactly() {
    let validator = jsonschema::validator_for(&machine_schema()).expect("schema compiles");

    for (name, bytes, is_success) in GOLDENS {
        let value: Value = serde_json::from_slice(bytes).unwrap();
        if let Err(error) = validator.validate(&value) {
            panic!("{name} failed schema validation: {error}");
        }

        let serialized = if *is_success {
            serde_json::to_value(serde_json::from_slice::<JsonV1Success>(bytes).unwrap()).unwrap()
        } else {
            serde_json::to_value(serde_json::from_slice::<JsonV1Failure>(bytes).unwrap()).unwrap()
        };
        assert_eq!(serialized, value, "{name} did not round-trip exactly");
    }
}

#[test]
fn focused_data_schemas_compile_and_validate_matching_goldens() {
    let inbox_schema: Value =
        serde_json::from_str(include_str!("../../../schemas/inbox-data-v1.schema.json")).unwrap();
    let status_schema: Value =
        serde_json::from_str(include_str!("../../../schemas/status-data-v1.schema.json")).unwrap();
    let inbox_validator = jsonschema::validator_for(&inbox_schema).unwrap();
    let status_validator = jsonschema::validator_for(&status_schema).unwrap();

    for bytes in [
        include_bytes!("../../../tests/fixtures/json-v1/inbox-success.json").as_slice(),
        include_bytes!("../../../tests/fixtures/json-v1/inbox-incomplete.json").as_slice(),
    ] {
        let envelope: Value = serde_json::from_slice(bytes).unwrap();
        assert!(inbox_validator.is_valid(&envelope["data"]));
    }
    for bytes in [
        include_bytes!("../../../tests/fixtures/json-v1/status-success.json").as_slice(),
        include_bytes!("../../../tests/fixtures/json-v1/status-blocked.json").as_slice(),
    ] {
        let envelope: Value = serde_json::from_slice(bytes).unwrap();
        assert!(status_validator.is_valid(&envelope["data"]));
    }
}

#[test]
fn unknown_object_fields_are_rejected() {
    let mut value: Value = serde_json::from_slice(GOLDENS[0].1).unwrap();
    value["data"]["unexpected"] = json!(true);
    assert_rejected_by_schema_and_rust(&value);
}

#[test]
fn omitted_nullable_fields_are_rejected() {
    let mut value: Value = serde_json::from_slice(include_bytes!(
        "../../../tests/fixtures/json-v1/status-success.json"
    ))
    .unwrap();
    value["data"]
        .as_object_mut()
        .unwrap()
        .remove("primary_blocker");
    assert_rejected_by_schema_and_rust(&value);
}

#[test]
fn unknown_command_discriminators_are_rejected() {
    let mut value: Value = serde_json::from_slice(GOLDENS[0].1).unwrap();
    value["command"] = json!("add.unknown");
    assert_rejected_by_schema_and_rust(&value);
}

#[test]
fn unknown_enum_values_are_rejected() {
    let mut value: Value = serde_json::from_slice(GOLDENS[0].1).unwrap();
    value["data"]["add_outcome"] = json!("updated");
    assert_rejected_by_schema_and_rust(&value);
}

#[test]
fn command_and_success_data_must_match() {
    let mut value: Value = serde_json::from_slice(GOLDENS[0].1).unwrap();
    value["command"] = json!("doctor");
    assert_rejected_by_schema_and_rust(&value);
}
