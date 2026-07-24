use serde_json::Value;

fn schema() -> Value {
    serde_json::from_str(include_str!(
        "../../../schemas/delivery/v1/delivery-package.schema.json"
    ))
    .expect("delivery package schema is valid JSON")
}

fn fixture(contents: &str) -> Value {
    serde_json::from_str(contents).expect("delivery fixture is valid JSON")
}

#[test]
fn delivery_package_schema_compiles_and_accepts_source_examples() {
    let validator = jsonschema::validator_for(&schema()).expect("delivery schema compiles");

    let mko = fixture(include_str!(
        "../../../tests/fixtures/delivery/v1/mko-knowledge-publication.json"
    ));
    let project2035 = fixture(include_str!(
        "../../../tests/fixtures/delivery/v1/project2035-decision-review-request.json"
    ));

    assert!(
        validator.is_valid(&mko),
        "MKO knowledge publication fixture must validate"
    );
    assert!(
        validator.is_valid(&project2035),
        "Project 2035 decision review fixture must validate"
    );
}

#[test]
fn delivery_package_rejects_plain_approval_flags() {
    let validator = jsonschema::validator_for(&schema()).expect("delivery schema compiles");
    let invalid = fixture(include_str!(
        "../../../tests/fixtures/delivery/v1/invalid-plain-approval.json"
    ));

    assert!(
        !validator.is_valid(&invalid),
        "a plain approval_status flag must not authorize delivery"
    );
}

#[test]
fn delivery_package_rejects_unknown_fields_and_domain_actions() {
    let validator = jsonschema::validator_for(&schema()).expect("delivery schema compiles");
    let mut package = fixture(include_str!(
        "../../../tests/fixtures/delivery/v1/project2035-decision-review-request.json"
    ));

    package
        .as_object_mut()
        .expect("fixture is an object")
        .insert("api_token".to_owned(), Value::String("secret".to_owned()));
    assert!(
        !validator.is_valid(&package),
        "unknown credential-shaped fields must be rejected"
    );

    let mut package = fixture(include_str!(
        "../../../tests/fixtures/delivery/v1/project2035-decision-review-request.json"
    ));
    package["interaction_policy"]["allowed_actions"] =
        Value::Array(vec![Value::String("buy".to_owned())]);
    assert!(
        !validator.is_valid(&package),
        "shared DeliveryPackage must reject investment actions"
    );
}
