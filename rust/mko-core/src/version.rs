use crate::error::MkoError;

pub const PRODUCT_VERSION: &str = env!("CARGO_PKG_VERSION");
pub const KNOWLEDGE_CONTRACT_VERSION: &str = "0.1.0";

pub fn supports_contract(version: &str) -> bool {
    version == KNOWLEDGE_CONTRACT_VERSION
}

/// The CLI and the installed Skill are one contract: the installer ships them
/// from the same checkout, so any version difference means a stale half.
pub fn verify_skill_version(skill_version: &str) -> Result<(), MkoError> {
    if skill_version == PRODUCT_VERSION {
        return Ok(());
    }
    Err(MkoError::new(
        "skill_version_mismatch",
        format!(
            "the Skill declares version {skill_version} but this mko CLI is {PRODUCT_VERSION}; \
             stop using mko in this session, reinstall the CLI and Skill together from the \
             repository installer (scripts/install.sh --yes or scripts/install.ps1 -Yes), then \
             restart the agent session"
        ),
    ))
}

#[cfg(test)]
mod tests {
    use super::{PRODUCT_VERSION, verify_skill_version};

    #[test]
    fn matching_skill_version_passes_the_handshake() {
        verify_skill_version(PRODUCT_VERSION).unwrap();
    }

    #[test]
    fn any_other_skill_version_is_a_typed_mismatch() {
        for stale in ["0.0.0", "0.3.0-old", " ", ""] {
            let error = verify_skill_version(stale).unwrap_err();
            assert_eq!(error.code(), "skill_version_mismatch");
            assert!(error.message().contains(PRODUCT_VERSION));
            assert!(error.message().contains("reinstall"));
        }
    }
}
