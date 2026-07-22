use std::{
    env,
    ffi::{OsStr, OsString},
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};

use crate::{
    config::KnowledgeConfig,
    config_v2::KnowledgeConfigV2,
    error::MkoError,
    path_policy::canonical_directory,
    profile::{PersonalProfile, ProfileStore},
};

struct PersonalKnowledgeDescriptor {
    provider_type: String,
    root_env: String,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Scope {
    Personal,
}

impl Scope {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Personal => "personal",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ContextSource {
    Explicit,
    Ancestor,
    Profile,
}

const UNPROFILED_CONTEXT_NAME: &str = "unprofiled";

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResolvedPersonalContext {
    pub repository_root: PathBuf,
    pub provider_root: PathBuf,
    pub provider_type: String,
    pub profile_name: String,
    pub scope: Scope,
    pub source: ContextSource,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ResolveContextRequest {
    explicit_repository_root: Option<PathBuf>,
    explicit_scope: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum SelectedPersonalContext {
    Repository {
        repository_root: PathBuf,
        source: ContextSource,
    },
    Profile {
        profile_name: String,
        profile: PersonalProfile,
    },
}

impl ResolveContextRequest {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_explicit_repository(mut self, repository_root: impl AsRef<Path>) -> Self {
        self.explicit_repository_root = Some(repository_root.as_ref().to_path_buf());
        self
    }

    pub fn with_explicit_scope(mut self, scope: impl Into<String>) -> Self {
        self.explicit_scope = Some(scope.into());
        self
    }
}

pub trait PlatformEnvironment {
    fn config_home(&self) -> Result<PathBuf, MkoError>;
    fn home_dir(&self) -> Result<PathBuf, MkoError>;
    fn current_dir(&self) -> Result<PathBuf, MkoError>;
    fn environment_value(&self, name: &OsStr) -> Option<OsString>;
}

#[derive(Clone, Copy, Debug, Default)]
pub struct SystemPlatformEnvironment;

impl PlatformEnvironment for SystemPlatformEnvironment {
    fn config_home(&self) -> Result<PathBuf, MkoError> {
        platform_config_home()
    }

    fn home_dir(&self) -> Result<PathBuf, MkoError> {
        home_directory()
    }

    fn current_dir(&self) -> Result<PathBuf, MkoError> {
        env::current_dir().map_err(|error| {
            MkoError::new(
                "current_directory_unavailable",
                format!("cannot determine current directory: {error}"),
            )
        })
    }

    fn environment_value(&self, name: &OsStr) -> Option<OsString> {
        env::var_os(name)
    }
}

pub fn resolve_personal_context(
    request: ResolveContextRequest,
    platform: &dyn PlatformEnvironment,
) -> Result<ResolvedPersonalContext, MkoError> {
    match select_personal_context(request, platform)? {
        SelectedPersonalContext::Repository {
            repository_root,
            source,
        } => resolve_unprofiled_context(&repository_root, source, platform),
        SelectedPersonalContext::Profile {
            profile_name,
            profile,
        } => resolve_profile_context(&profile_name, &profile),
    }
}

pub(crate) fn select_personal_context(
    request: ResolveContextRequest,
    platform: &dyn PlatformEnvironment,
) -> Result<SelectedPersonalContext, MkoError> {
    if let Some(explicit_scope) = request.explicit_scope.as_deref()
        && explicit_scope != Scope::Personal.as_str()
    {
        return Err(MkoError::new(
            "scope_conflict",
            "explicit scope conflicts with Personal context resolution",
        ));
    }

    if let Some(repository_root) = request.explicit_repository_root {
        return Ok(SelectedPersonalContext::Repository {
            repository_root,
            source: ContextSource::Explicit,
        });
    }

    let current_dir = platform.current_dir()?;
    if let Some(repository_root) = ancestor_knowledge_base(&current_dir)? {
        return Ok(SelectedPersonalContext::Repository {
            repository_root,
            source: ContextSource::Ancestor,
        });
    }

    let store = ProfileStore::from_platform(platform)?;
    let Some(profiles) = store.read()? else {
        return Err(MkoError::new(
            "context_not_found",
            "provide a repository, work inside a knowledge base, or configure a default profile",
        ));
    };
    let profile = profiles
        .profiles
        .get(&profiles.default_profile)
        .ok_or_else(|| {
            MkoError::new("profile_invalid", "default machine profile does not exist")
        })?;

    Ok(SelectedPersonalContext::Profile {
        profile_name: profiles.default_profile.clone(),
        profile: profile.clone(),
    })
}

fn resolve_unprofiled_context(
    selected_repository: &Path,
    source: ContextSource,
    platform: &dyn PlatformEnvironment,
) -> Result<ResolvedPersonalContext, MkoError> {
    let repository_root = canonical_directory(selected_repository, "repository_root_invalid")?;
    let knowledge = validated_personal_knowledge(&repository_root)?;
    let provider_root = provider_root_from_environment(&knowledge, platform)?;

    Ok(ResolvedPersonalContext {
        repository_root,
        provider_root,
        provider_type: knowledge.provider_type,
        profile_name: UNPROFILED_CONTEXT_NAME.into(),
        scope: Scope::Personal,
        source,
    })
}

fn resolve_profile_context(
    profile_name: &str,
    profile: &PersonalProfile,
) -> Result<ResolvedPersonalContext, MkoError> {
    let repository_root = canonical_directory(&profile.repository_root, "repository_root_invalid")?;
    let knowledge = validated_personal_knowledge(&repository_root)?;
    let provider_root = profile_provider_root(profile)?;

    Ok(ResolvedPersonalContext {
        repository_root,
        provider_root,
        provider_type: knowledge.provider_type,
        profile_name: profile_name.into(),
        scope: Scope::Personal,
        source: ContextSource::Profile,
    })
}

fn validated_personal_knowledge(
    repository_root: &Path,
) -> Result<PersonalKnowledgeDescriptor, MkoError> {
    if let Ok(knowledge) = KnowledgeConfigV2::read(repository_root) {
        return Ok(PersonalKnowledgeDescriptor {
            provider_type: knowledge.provider.r#type,
            root_env: knowledge.provider.root_env,
        });
    }
    let knowledge = KnowledgeConfig::read(repository_root)?;
    if knowledge.scope != Scope::Personal.as_str() {
        return Err(MkoError::new(
            "scope_conflict",
            "resolved knowledge base is not Personal scope",
        ));
    }
    Ok(PersonalKnowledgeDescriptor {
        provider_type: knowledge.provider.r#type,
        root_env: knowledge.provider.root_env,
    })
}

fn ancestor_knowledge_base(current_dir: &Path) -> Result<Option<PathBuf>, MkoError> {
    let current_dir = canonical_directory(current_dir, "current_directory_invalid")?;
    for ancestor in current_dir.ancestors() {
        let marker = ancestor.join("knowledge-os.yaml");
        match std::fs::symlink_metadata(&marker) {
            Ok(metadata) if metadata.is_file() && !metadata.file_type().is_symlink() => {
                validated_personal_knowledge(ancestor)?;
                return Ok(Some(ancestor.to_path_buf()));
            }
            Ok(_) => {
                return Err(MkoError::new(
                    "context_marker_invalid",
                    "knowledge-os.yaml must be a regular file and must not be a symlink",
                ));
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(MkoError::new(
                    "context_discovery_failed",
                    format!("cannot inspect {}: {error}", marker.display()),
                ));
            }
        }
    }
    Ok(None)
}

fn profile_provider_root(profile: &PersonalProfile) -> Result<PathBuf, MkoError> {
    canonical_directory(&profile.provider_root, "provider_root_invalid")
}

fn provider_root_from_environment(
    knowledge: &PersonalKnowledgeDescriptor,
    platform: &dyn PlatformEnvironment,
) -> Result<PathBuf, MkoError> {
    let value = platform
        .environment_value(OsStr::new(&knowledge.root_env))
        .ok_or_else(|| {
            MkoError::new(
                "provider_root_missing",
                format!(
                    "set {} or select a machine profile for this repository",
                    knowledge.root_env
                ),
            )
        })?;
    canonical_directory(Path::new(&value), "provider_root_invalid")
}

#[cfg(target_os = "windows")]
fn platform_config_home() -> Result<PathBuf, MkoError> {
    environment_path("APPDATA", "configuration home")
}

#[cfg(target_os = "macos")]
fn platform_config_home() -> Result<PathBuf, MkoError> {
    Ok(home_directory()?.join("Library/Application Support"))
}

#[cfg(not(any(target_os = "windows", target_os = "macos")))]
fn platform_config_home() -> Result<PathBuf, MkoError> {
    Ok(env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or(home_directory()?.join(".config")))
}

#[cfg(target_os = "windows")]
fn home_directory() -> Result<PathBuf, MkoError> {
    environment_path("USERPROFILE", "home directory")
}

#[cfg(not(target_os = "windows"))]
fn home_directory() -> Result<PathBuf, MkoError> {
    environment_path("HOME", "home directory")
}

fn environment_path(name: &str, description: &str) -> Result<PathBuf, MkoError> {
    env::var_os(name).map(PathBuf::from).ok_or_else(|| {
        MkoError::new(
            "platform_environment_unavailable",
            format!("cannot determine {description}: {name} is not set"),
        )
    })
}
