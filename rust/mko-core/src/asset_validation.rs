use crate::{
    model::{AssetRecord, Classification},
    path_policy::validate_portable_relative_path,
    state::validate_asset_state,
};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AssetValidationIssue {
    pub code: String,
    pub message: String,
}

pub fn validate_canonical_asset(path: &str, asset: &AssetRecord) -> Vec<AssetValidationIssue> {
    let mut issues = Vec::new();
    let hash = valid_prefixed_hash(&asset.id, "personal-asset-");
    let expected_path = format!("assets/registry/{}.md", asset.id);
    if hash.is_none()
        || path != expected_path
        || asset.record_type != "asset"
        || asset.schema_version != 1
        || asset.scope != "personal"
        || asset.classification != Classification::Personal
        || asset.asset_class != "document"
        || asset.media_type != "application/pdf"
        || asset.provider.r#type != "google-drive-stream"
        || asset.fingerprint.method != "sha256"
        || hash.is_some_and(|hash| asset.fingerprint.value != format!("sha256:{hash}"))
    {
        issues.push(AssetValidationIssue {
            code: "registry_invalid".into(),
            message: "Asset identity, Scope, schema, path, or fingerprint is not canonical".into(),
        });
    }
    if validate_portable_relative_path(&asset.provider.locator).is_err() {
        issues.push(AssetValidationIssue {
            code: "path_not_portable".into(),
            message: "Provider locator must be a portable relative path".into(),
        });
    }
    if let Err(error) = validate_asset_state(asset) {
        issues.push(AssetValidationIssue {
            code: error.code().into(),
            message: error.message().into(),
        });
    }
    issues
}

fn valid_prefixed_hash<'a>(id: &'a str, prefix: &str) -> Option<&'a str> {
    id.strip_prefix(prefix).filter(|hash| {
        hash.len() == 64
            && hash
                .bytes()
                .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
    })
}
