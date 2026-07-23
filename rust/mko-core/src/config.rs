use std::{
    env, fs,
    path::{Path, PathBuf},
};

use serde::Deserialize;

use crate::{
    context::{ResolvedPersonalContext, Scope},
    error::MkoError,
    path_policy::canonical_directory,
    safe_yaml::validate_yaml_input,
    version::KNOWLEDGE_CONTRACT_VERSION,
};

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct KnowledgeConfig {
    pub system: String,
    pub scope: String,
    pub core_version: String,
    pub schema_version: u32,
    pub provider: ProviderConfig,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderConfig {
    pub name: String,
    pub r#type: String,
    pub root_env: String,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LocalConfig {
    pub provider_root: PathBuf,
}

#[derive(Clone, Debug)]
pub struct CaptureConfig {
    pub repository_root: PathBuf,
    pub provider_root: PathBuf,
    pub provider_type: String,
}

impl CaptureConfig {
    pub fn from_resolved_context(context: &ResolvedPersonalContext) -> Result<Self, MkoError> {
        if context.scope != Scope::Personal || context.profile_name.trim().is_empty() {
            return Err(MkoError::new(
                "context_invalid",
                "resolved context must name a Personal profile",
            ));
        }
        let repository_root =
            canonical_directory(&context.repository_root, "repository_root_invalid")?;
        let provider_root = canonical_directory(&context.provider_root, "provider_root_invalid")?;
        let knowledge = KnowledgeConfig::read(&repository_root)?;
        if knowledge.scope != context.scope.as_str()
            || knowledge.provider.r#type != context.provider_type
        {
            return Err(MkoError::new(
                "context_invalid",
                "resolved context does not match knowledge-os.yaml",
            ));
        }
        Ok(Self {
            repository_root,
            provider_root,
            provider_type: knowledge.provider.r#type,
        })
    }
}

impl KnowledgeConfig {
    pub fn read(repository_root: &Path) -> Result<Self, MkoError> {
        let path = repository_root.join("knowledge-os.yaml");
        let input = read_config(&path)?;
        let config: Self = parse_config(&input)?;
        config.validate()?;
        Ok(config)
    }

    fn validate(&self) -> Result<(), MkoError> {
        if self.system != "my-knowledge-os"
            || self.scope != "personal"
            || self.core_version != KNOWLEDGE_CONTRACT_VERSION
            || self.schema_version != 1
            || self.provider.name.trim().is_empty()
            || self.provider.r#type != "google-drive-stream"
            || self.provider.root_env.trim().is_empty()
        {
            return Err(MkoError::new(
                "config_invalid",
                "knowledge-os.yaml is not compatible with My Knowledge OS Core 0.1.0",
            ));
        }
        Ok(())
    }
}

impl LocalConfig {
    pub fn read(path: &Path) -> Result<Self, MkoError> {
        let input = read_config(path)?;
        let mut config: Self = parse_config(&input)?;
        if config.provider_root.as_os_str().is_empty() {
            return Err(MkoError::new(
                "config_invalid",
                "local configuration must set provider_root",
            ));
        }
        if config.provider_root.is_relative() {
            let parent = path.parent().ok_or_else(|| {
                MkoError::new(
                    "config_invalid",
                    "local configuration has no parent directory",
                )
            })?;
            config.provider_root = parent.join(&config.provider_root);
        }
        Ok(config)
    }
}

pub fn load_capture_config(
    repository_root: &Path,
    local_config_path: Option<&Path>,
) -> Result<CaptureConfig, MkoError> {
    let repository_root = canonical_directory(repository_root, "repository_root_invalid")?;
    let knowledge = KnowledgeConfig::read(&repository_root)?;
    let local_config_path = local_config_path
        .map(PathBuf::from)
        .or_else(|| env::var_os("MKO_LOCAL_CONFIG").map(PathBuf::from))
        .ok_or_else(|| {
            MkoError::new(
                "local_config_missing",
                "provide --local-config or set MKO_LOCAL_CONFIG",
            )
        })?;
    let local = LocalConfig::read(&local_config_path)?;

    Ok(CaptureConfig {
        repository_root,
        provider_root: canonical_directory(&local.provider_root, "provider_root_invalid")?,
        provider_type: knowledge.provider.r#type,
    })
}

fn read_config(path: &Path) -> Result<String, MkoError> {
    fs::read_to_string(path).map_err(|error| {
        MkoError::new(
            "config_unreadable",
            format!("cannot read configuration {}: {error}", path.display()),
        )
    })
}

fn parse_config<T>(input: &str) -> Result<T, MkoError>
where
    T: for<'de> Deserialize<'de>,
{
    validate_yaml_input(input)?;
    serde_saphyr::from_str(input)
        .map_err(|error| MkoError::new("config_invalid", error.to_string()))
}
