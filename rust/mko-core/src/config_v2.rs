use std::{fs, path::Path};

use serde::{Deserialize, Serialize};

use crate::{error::MkoError, safe_yaml::validate_yaml_input};

pub const CONTRACT_VERSION_V2: &str = "0.3.0";
pub const SCHEMA_VERSION_V2: u32 = 2;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DerivedArtifactsPolicyV2 {
    LocalOnly,
    Provider,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DomainPolicyV2 {
    Standard,
    HighRisk,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderConfigV2 {
    pub name: String,
    pub r#type: String,
    pub root_env: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DomainPoliciesV2 {
    pub default: DomainPolicyV2,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct KnowledgeConfigV2 {
    pub system: String,
    pub contract_version: String,
    pub schema_version: u32,
    pub scope: String,
    pub provider: ProviderConfigV2,
    pub derived_artifacts: DerivedArtifactsPolicyV2,
    pub domain_policies: DomainPoliciesV2,
}

impl KnowledgeConfigV2 {
    pub fn personal_default() -> Self {
        Self {
            system: "my-knowledge-os".into(),
            contract_version: CONTRACT_VERSION_V2.into(),
            schema_version: SCHEMA_VERSION_V2,
            scope: "personal".into(),
            provider: ProviderConfigV2 {
                name: "personal_assets".into(),
                r#type: "google-drive-filesystem".into(),
                root_env: "MKO_PERSONAL_PROVIDER_ROOT".into(),
            },
            derived_artifacts: DerivedArtifactsPolicyV2::LocalOnly,
            domain_policies: DomainPoliciesV2 {
                default: DomainPolicyV2::Standard,
            },
        }
    }

    pub fn read(repository_root: &Path) -> Result<Self, MkoError> {
        let path = repository_root.join("knowledge-os.yaml");
        let metadata = fs::symlink_metadata(&path).map_err(|error| {
            MkoError::new(
                "kb_config_unreadable",
                format!("cannot inspect knowledge-os.yaml: {error}"),
            )
        })?;
        if !metadata.file_type().is_file() || metadata.file_type().is_symlink() {
            return Err(MkoError::new(
                "kb_config_invalid",
                "knowledge-os.yaml must be a regular file",
            ));
        }
        let input = fs::read_to_string(&path).map_err(|error| {
            MkoError::new(
                "kb_config_unreadable",
                format!("cannot read knowledge-os.yaml: {error}"),
            )
        })?;
        validate_yaml_input(&input)?;
        let config: Self = serde_saphyr::from_str(&input)
            .map_err(|error| MkoError::new("kb_config_invalid", error.to_string()))?;
        config.validate()?;
        Ok(config)
    }

    pub fn validate(&self) -> Result<(), MkoError> {
        if self.system != "my-knowledge-os"
            || self.contract_version != CONTRACT_VERSION_V2
            || self.schema_version != SCHEMA_VERSION_V2
            || self.scope != "personal"
        {
            return Err(MkoError::new(
                "kb_schema_unsupported",
                "select or create a My Knowledge OS v0.3 Personal KB",
            ));
        }
        if self.provider.name.trim().is_empty()
            || self.provider.r#type != "google-drive-filesystem"
            || self.provider.root_env.trim().is_empty()
        {
            return Err(MkoError::new(
                "kb_config_invalid",
                "the v0.3 Personal provider configuration is invalid",
            ));
        }
        Ok(())
    }

    pub fn render(&self) -> Result<Vec<u8>, MkoError> {
        let text = serde_saphyr::to_string(self)
            .map_err(|error| MkoError::new("kb_config_invalid", error.to_string()))?;
        Ok(text.replace("\r\n", "\n").replace('\r', "\n").into_bytes())
    }
}
