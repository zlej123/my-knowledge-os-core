use chrono::{DateTime, Utc};
use serde::{Deserialize, Deserializer, Serialize};

use crate::{error::MkoError, fingerprint::MAX_ASSET_BYTES};

pub const MAX_CAPTURE_ENVELOPE_BYTES_V1: usize = 64 * 1024;
pub const MAX_TEXT_INPUT_BYTES_V1: usize = 16 * 1024;
pub const MAX_CLASSIFIER_PROPOSAL_BYTES_V1: usize = 16 * 1024;
pub const MAX_PDF_DECLARED_BYTES_V1: u64 = MAX_ASSET_BYTES;
pub const HIGH_CONFIDENCE_MINIMUM_V1: u8 = 80;

fn deserialize_schema_version<'de, D>(deserializer: D) -> Result<u32, D::Error>
where
    D: Deserializer<'de>,
{
    let version = u32::deserialize(deserializer)?;
    if version == 1 {
        Ok(version)
    } else {
        Err(serde::de::Error::custom("schema_version must be 1"))
    }
}

/// Parses one bounded CaptureEnvelope v1. Parsing and routing are deliberately
/// pure: this module does not fetch Telegram content or write any domain state.
pub fn parse_capture_envelope_v1(input: &[u8]) -> Result<CaptureEnvelopeV1, MkoError> {
    if input.len() > MAX_CAPTURE_ENVELOPE_BYTES_V1 {
        return Err(MkoError::new(
            "capture_envelope_too_large",
            "CaptureEnvelope exceeds the bounded v1 input size",
        ));
    }
    let envelope: CaptureEnvelopeV1 = serde_json::from_slice(input)
        .map_err(|error| MkoError::new("capture_envelope_invalid", error.to_string()))?;
    envelope.validate()?;
    Ok(envelope)
}

/// Parses one bounded, untrusted classifier proposal for the pure router.
pub fn parse_classifier_proposal_v1(input: &[u8]) -> Result<ClassifierProposalV1, MkoError> {
    if input.len() > MAX_CLASSIFIER_PROPOSAL_BYTES_V1 {
        return Err(MkoError::new(
            "routing_proposal_too_large",
            "classifier proposal exceeds the bounded v1 input size",
        ));
    }
    let proposal: ClassifierProposalV1 = serde_json::from_slice(input)
        .map_err(|error| MkoError::new("routing_proposal_invalid", error.to_string()))?;
    proposal.validate()?;
    Ok(proposal)
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CaptureChannelV1 {
    Telegram,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SubjectScopeV1 {
    General,
    Finance,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ChannelIdentityV1 {
    pub profile_id: String,
    pub chat_id: String,
    pub sender_id: String,
    pub update_id: String,
    pub message_id: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "input_type", rename_all = "snake_case", deny_unknown_fields)]
pub enum CaptureInputV1 {
    TelegramPdf {
        file_id: String,
        file_unique_id: String,
        declared_size_bytes: u64,
        mime_type: String,
    },
    Youtube {
        video_id: String,
        canonical_url: String,
    },
    Text {
        text: String,
    },
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CaptureEnvelopeV1 {
    #[serde(deserialize_with = "deserialize_schema_version")]
    pub schema_version: u32,
    pub capture_id: String,
    pub channel: CaptureChannelV1,
    pub channel_identity: ChannelIdentityV1,
    pub input: CaptureInputV1,
    #[serde(default)]
    pub selected_scope: Option<SubjectScopeV1>,
    pub received_at: DateTime<Utc>,
}

impl CaptureEnvelopeV1 {
    pub fn validate(&self) -> Result<(), MkoError> {
        if self.schema_version != 1 {
            return Err(MkoError::new(
                "capture_envelope_invalid",
                "CaptureEnvelope schema_version must be 1",
            ));
        }
        if !is_capture_id(&self.capture_id) {
            return Err(MkoError::new(
                "capture_envelope_invalid",
                "capture_id must be a bounded cap_ identifier",
            ));
        }
        validate_channel_identity(&self.channel_identity)?;
        validate_capture_input(&self.input)
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ClassifierProposalV1 {
    pub proposed_scope: SubjectScopeV1,
    pub confidence: u8,
    pub mixed_subjects: bool,
    pub conflicting: bool,
}

impl ClassifierProposalV1 {
    pub fn validate(&self) -> Result<(), MkoError> {
        if self.confidence > 100 {
            return Err(MkoError::new(
                "routing_proposal_invalid",
                "classifier confidence must be between 0 and 100",
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RoutingAuthorityV1 {
    UserSelected,
    UserConfirmedProposal,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RouteOutcomeKindV1 {
    ReadyGeneral,
    ReadyFinance,
    GeneralConfirmationRequired,
    FinanceConfirmationRequired,
    RoutingConfirmationRequired,
    Rejected,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RoutingNextActionV1 {
    ConfirmGeneral,
    ConfirmFinance,
    ChooseScope,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RouteRejectionReasonV1 {
    ConfirmationRequiresProposal,
    ConfirmationDoesNotMatchProposal,
    ConfirmationCannotResolveMixedOrConflictingProposal,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct RouteOutcomeV1 {
    pub outcome: RouteOutcomeKindV1,
    pub confirmed_scope: Option<SubjectScopeV1>,
    pub routing_authority: Option<RoutingAuthorityV1>,
    pub proposal: Option<ClassifierProposalV1>,
    pub next_action: Option<RoutingNextActionV1>,
    pub rejection_reason: Option<RouteRejectionReasonV1>,
}

/// Resolves only the routing contract. It cannot create Assets, Source or
/// Knowledge records, invoke Delivery, or create a Project 2035 decision.
pub fn resolve_route_v1(
    envelope: &CaptureEnvelopeV1,
    classifier_proposal: Option<&ClassifierProposalV1>,
    confirmed_proposal_scope: Option<SubjectScopeV1>,
) -> Result<RouteOutcomeV1, MkoError> {
    envelope.validate()?;
    if let Some(proposal) = classifier_proposal {
        proposal.validate()?;
    }

    if let Some(scope) = &envelope.selected_scope {
        return Ok(ready(scope.clone(), RoutingAuthorityV1::UserSelected));
    }

    if let Some(confirmed_scope) = confirmed_proposal_scope {
        let Some(proposal) = classifier_proposal else {
            return Ok(rejected(
                RouteRejectionReasonV1::ConfirmationRequiresProposal,
            ));
        };
        if proposal.mixed_subjects || proposal.conflicting {
            return Ok(rejected(
                RouteRejectionReasonV1::ConfirmationCannotResolveMixedOrConflictingProposal,
            ));
        }
        if proposal.proposed_scope != confirmed_scope {
            return Ok(rejected(
                RouteRejectionReasonV1::ConfirmationDoesNotMatchProposal,
            ));
        }
        return Ok(ready(
            confirmed_scope,
            RoutingAuthorityV1::UserConfirmedProposal,
        ));
    }

    let Some(proposal) = classifier_proposal else {
        return Ok(confirmation_required(
            RouteOutcomeKindV1::RoutingConfirmationRequired,
            None,
            RoutingNextActionV1::ChooseScope,
        ));
    };
    if proposal.mixed_subjects || proposal.conflicting {
        return Ok(confirmation_required(
            RouteOutcomeKindV1::RoutingConfirmationRequired,
            Some(proposal.clone()),
            RoutingNextActionV1::ChooseScope,
        ));
    }
    match proposal.proposed_scope {
        SubjectScopeV1::Finance if proposal.confidence >= HIGH_CONFIDENCE_MINIMUM_V1 => {
            Ok(confirmation_required(
                RouteOutcomeKindV1::FinanceConfirmationRequired,
                Some(proposal.clone()),
                RoutingNextActionV1::ConfirmFinance,
            ))
        }
        SubjectScopeV1::Finance => Ok(confirmation_required(
            RouteOutcomeKindV1::RoutingConfirmationRequired,
            Some(proposal.clone()),
            RoutingNextActionV1::ChooseScope,
        )),
        SubjectScopeV1::General if proposal.confidence >= HIGH_CONFIDENCE_MINIMUM_V1 => {
            Ok(confirmation_required(
                RouteOutcomeKindV1::GeneralConfirmationRequired,
                Some(proposal.clone()),
                RoutingNextActionV1::ConfirmGeneral,
            ))
        }
        SubjectScopeV1::General => Ok(confirmation_required(
            RouteOutcomeKindV1::RoutingConfirmationRequired,
            Some(proposal.clone()),
            RoutingNextActionV1::ChooseScope,
        )),
    }
}

fn ready(scope: SubjectScopeV1, authority: RoutingAuthorityV1) -> RouteOutcomeV1 {
    let outcome = match scope {
        SubjectScopeV1::General => RouteOutcomeKindV1::ReadyGeneral,
        SubjectScopeV1::Finance => RouteOutcomeKindV1::ReadyFinance,
    };
    RouteOutcomeV1 {
        outcome,
        confirmed_scope: Some(scope),
        routing_authority: Some(authority),
        proposal: None,
        next_action: None,
        rejection_reason: None,
    }
}

fn confirmation_required(
    outcome: RouteOutcomeKindV1,
    proposal: Option<ClassifierProposalV1>,
    next_action: RoutingNextActionV1,
) -> RouteOutcomeV1 {
    RouteOutcomeV1 {
        outcome,
        confirmed_scope: None,
        routing_authority: None,
        proposal,
        next_action: Some(next_action),
        rejection_reason: None,
    }
}

fn rejected(reason: RouteRejectionReasonV1) -> RouteOutcomeV1 {
    RouteOutcomeV1 {
        outcome: RouteOutcomeKindV1::Rejected,
        confirmed_scope: None,
        routing_authority: None,
        proposal: None,
        next_action: None,
        rejection_reason: Some(reason),
    }
}

fn validate_channel_identity(identity: &ChannelIdentityV1) -> Result<(), MkoError> {
    if !is_profile_id(&identity.profile_id)
        || !is_telegram_id(&identity.chat_id, true)
        || !is_telegram_id(&identity.sender_id, false)
        || !is_telegram_id(&identity.update_id, false)
        || !is_telegram_id(&identity.message_id, false)
    {
        return Err(MkoError::new(
            "capture_envelope_invalid",
            "channel identity is not a bounded Telegram identity",
        ));
    }
    Ok(())
}

fn validate_capture_input(input: &CaptureInputV1) -> Result<(), MkoError> {
    match input {
        CaptureInputV1::TelegramPdf {
            file_id,
            file_unique_id,
            declared_size_bytes,
            mime_type,
        } => {
            if !is_opaque_reference(file_id)
                || !is_opaque_reference(file_unique_id)
                || *declared_size_bytes == 0
                || *declared_size_bytes > MAX_PDF_DECLARED_BYTES_V1
                || mime_type != "application/pdf"
            {
                return Err(MkoError::new(
                    "capture_envelope_invalid",
                    "Telegram PDF reference violates the bounded v1 contract",
                ));
            }
        }
        CaptureInputV1::Youtube {
            video_id,
            canonical_url,
        } => {
            let expected = format!("https://www.youtube.com/watch?v={video_id}");
            if !is_youtube_video_id(video_id) || canonical_url != &expected {
                return Err(MkoError::new(
                    "capture_envelope_invalid",
                    "YouTube input must use its canonical v1 URL and video ID",
                ));
            }
        }
        CaptureInputV1::Text { text } => {
            if text.is_empty() || text.len() > MAX_TEXT_INPUT_BYTES_V1 {
                return Err(MkoError::new(
                    "capture_envelope_invalid",
                    "text input exceeds the bounded v1 size",
                ));
            }
        }
    }
    Ok(())
}

fn is_capture_id(value: &str) -> bool {
    value
        .strip_prefix("cap_")
        .is_some_and(|suffix| suffix.len() <= 127 && is_lower_identifier(suffix))
}

fn is_profile_id(value: &str) -> bool {
    value.len() <= 64
        && value
            .chars()
            .next()
            .is_some_and(|character| character.is_ascii_lowercase())
        && value.chars().all(|character| {
            character.is_ascii_lowercase()
                || character.is_ascii_digit()
                || matches!(character, '_' | '-')
        })
}

fn is_lower_identifier(value: &str) -> bool {
    !value.is_empty()
        && value.chars().all(|character| {
            character.is_ascii_lowercase()
                || character.is_ascii_digit()
                || matches!(character, '_' | '-')
        })
}

fn is_telegram_id(value: &str, allow_negative: bool) -> bool {
    let digits = if allow_negative {
        value.strip_prefix('-').unwrap_or(value)
    } else {
        value
    };
    value.len() <= 20
        && !digits.is_empty()
        && !digits.starts_with('0')
        && digits.bytes().all(|byte| byte.is_ascii_digit())
}

fn is_opaque_reference(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 256
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
}

fn is_youtube_video_id(value: &str) -> bool {
    value.len() == 11
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
}
