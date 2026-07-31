use std::{fs::OpenOptions, io::Read, path::Path};

#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;
#[cfg(windows)]
use std::os::windows::fs::OpenOptionsExt;

use serde::{Deserialize, Serialize};

use crate::{error::MkoError, safe_yaml::validate_yaml_input};

pub const CONTRACT_VERSION_V2: &str = "0.3.0";
pub const SCHEMA_VERSION_V2: u32 = 2;
pub const DEFAULT_HYDRATION_WARNING_THRESHOLD_BYTES_V2: u64 = 10 * 1024 * 1024;
const MAX_CONFIG_BYTES: u64 = 64 * 1024;

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

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PerspectiveV2 {
    Life,
    Learning,
    Technical,
    Project,
    Investment,
}

impl PerspectiveV2 {
    pub fn all() -> &'static [Self] {
        &[
            Self::Life,
            Self::Learning,
            Self::Technical,
            Self::Project,
            Self::Investment,
        ]
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Life => "life",
            Self::Learning => "learning",
            Self::Technical => "technical",
            Self::Project => "project",
            Self::Investment => "investment",
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderConfigV2 {
    pub name: String,
    pub r#type: String,
    pub root_env: String,
    #[serde(default = "default_hydration_warning_threshold_bytes_v2")]
    pub hydration_warning_threshold_bytes: u64,
}

const fn default_hydration_warning_threshold_bytes_v2() -> u64 {
    DEFAULT_HYDRATION_WARNING_THRESHOLD_BYTES_V2
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
                hydration_warning_threshold_bytes: DEFAULT_HYDRATION_WARNING_THRESHOLD_BYTES_V2,
            },
            derived_artifacts: DerivedArtifactsPolicyV2::LocalOnly,
            domain_policies: DomainPoliciesV2 {
                default: DomainPolicyV2::Standard,
            },
        }
    }

    pub fn read(repository_root: &Path) -> Result<Self, MkoError> {
        let path = repository_root.join("knowledge-os.yaml");
        let mut options = OpenOptions::new();
        options.read(true);
        configure_nofollow(&mut options);
        let file = options.open(&path).map_err(|error| {
            MkoError::new(
                "kb_config_unreadable",
                format!("cannot open knowledge-os.yaml without following links: {error}"),
            )
        })?;
        let metadata = file.metadata().map_err(|error| {
            MkoError::new(
                "kb_config_unreadable",
                format!("cannot inspect open knowledge-os.yaml: {error}"),
            )
        })?;
        if !metadata.file_type().is_file()
            || metadata.file_type().is_symlink()
            || metadata.len() > MAX_CONFIG_BYTES
        {
            return Err(MkoError::new(
                "kb_config_invalid",
                "knowledge-os.yaml must be a bounded regular non-symlink file",
            ));
        }
        let mut bytes = Vec::with_capacity(metadata.len() as usize);
        file.take(MAX_CONFIG_BYTES + 1)
            .read_to_end(&mut bytes)
            .map_err(|error| {
                MkoError::new(
                    "kb_config_unreadable",
                    format!("cannot read knowledge-os.yaml: {error}"),
                )
            })?;
        if bytes.len() as u64 > MAX_CONFIG_BYTES {
            return Err(MkoError::new(
                "kb_config_invalid",
                "knowledge-os.yaml exceeds the bounded input size",
            ));
        }
        let input = String::from_utf8(bytes).map_err(|error| {
            MkoError::new(
                "kb_config_invalid",
                format!("knowledge-os.yaml is not UTF-8: {error}"),
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
            || self.provider.hydration_warning_threshold_bytes == 0
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

    pub fn policy_for_perspectives(&self, perspectives: &[PerspectiveV2]) -> DomainPolicyV2 {
        if self.domain_policies.default == DomainPolicyV2::HighRisk
            || perspectives.contains(&PerspectiveV2::Investment)
        {
            DomainPolicyV2::HighRisk
        } else {
            DomainPolicyV2::Standard
        }
    }
}

#[cfg(target_os = "linux")]
fn configure_nofollow(options: &mut OpenOptions) {
    const O_NONBLOCK: i32 = 0x800;
    const O_NOFOLLOW: i32 = 0x20_000;
    options.custom_flags(O_NOFOLLOW | O_NONBLOCK);
}

#[cfg(target_os = "macos")]
fn configure_nofollow(options: &mut OpenOptions) {
    const O_NONBLOCK: i32 = 0x4;
    const O_NOFOLLOW: i32 = 0x100;
    options.custom_flags(O_NOFOLLOW | O_NONBLOCK);
}

#[cfg(windows)]
fn configure_nofollow(options: &mut OpenOptions) {
    const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
    options.custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
}

#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
fn configure_nofollow(_options: &mut OpenOptions) {}
