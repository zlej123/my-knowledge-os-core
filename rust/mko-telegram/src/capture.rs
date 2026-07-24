#![forbid(unsafe_code)]

//! Pure, bounded normalization of one Telegram `Update` into an MKO
//! `CaptureEnvelopeV1`.
//!
//! This crate intentionally has no HTTP client, filesystem access, secret
//! lookup, Telegram token handling, cursor persistence, or domain writers.
//! The future Telegram pull adapter owns those side effects and may use this
//! crate only after it has obtained an untrusted Update payload.

use std::collections::BTreeSet;

use chrono::DateTime;
use mko_core::{
    capture_v1::{
        CaptureChannelV1, CaptureEnvelopeV1, CaptureInputV1, ChannelIdentityV1,
        MAX_CAPTURE_ENVELOPE_BYTES_V1, MAX_PDF_DECLARED_BYTES_V1, SubjectScopeV1,
    },
    error::MkoError,
};
use serde::Deserialize;
use sha2::{Digest, Sha256};

/// Bounds one untrusted Telegram Update before JSON parsing. This leaves room
/// for Telegram metadata while remaining compatible with the smaller
/// CaptureEnvelope limit enforced by `mko-core`.
pub const MAX_TELEGRAM_UPDATE_BYTES_V1: usize = MAX_CAPTURE_ENVELOPE_BYTES_V1;

/// Configures the local Telegram identity boundary for one MKO profile.
///
/// Both allowlists must be non-empty. An Update is accepted only when *both*
/// its chat ID and sender ID appear in the configured allowlists.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TelegramCaptureConfigV1 {
    pub profile_id: String,
    pub allowed_chat_ids: BTreeSet<String>,
    pub allowed_sender_ids: BTreeSet<String>,
}

impl TelegramCaptureConfigV1 {
    pub fn new(
        profile_id: impl Into<String>,
        allowed_chat_ids: impl IntoIterator<Item = impl Into<String>>,
        allowed_sender_ids: impl IntoIterator<Item = impl Into<String>>,
    ) -> Result<Self, MkoError> {
        let config = Self {
            profile_id: profile_id.into(),
            allowed_chat_ids: allowed_chat_ids.into_iter().map(Into::into).collect(),
            allowed_sender_ids: allowed_sender_ids.into_iter().map(Into::into).collect(),
        };
        config.validate()?;
        Ok(config)
    }

    pub fn validate(&self) -> Result<(), MkoError> {
        if !is_profile_id(&self.profile_id)
            || self.allowed_chat_ids.is_empty()
            || self.allowed_sender_ids.is_empty()
            || !self
                .allowed_chat_ids
                .iter()
                .all(|value| is_telegram_id(value, true))
            || !self
                .allowed_sender_ids
                .iter()
                .all(|value| is_telegram_id(value, false))
        {
            return Err(MkoError::new(
                "telegram_capture_config_invalid",
                "profile and non-empty Telegram identity allowlists must be valid",
            ));
        }
        Ok(())
    }
}

/// Parses exactly one untrusted Telegram `Update` object and returns a strict
/// `CaptureEnvelopeV1`. It does not perform network, filesystem, secret, or
/// knowledge-domain operations.
pub fn normalize_telegram_update_v1(
    config: &TelegramCaptureConfigV1,
    input: &[u8],
) -> Result<CaptureEnvelopeV1, MkoError> {
    config.validate()?;
    if input.len() > MAX_TELEGRAM_UPDATE_BYTES_V1 {
        return Err(MkoError::new(
            "telegram_update_too_large",
            "Telegram Update exceeds the bounded v1 input size",
        ));
    }

    let update: TelegramUpdate = serde_json::from_slice(input)
        .map_err(|error| MkoError::new("telegram_update_invalid", error.to_string()))?;
    let message = update.message.ok_or_else(|| {
        MkoError::new(
            "telegram_update_unsupported",
            "Telegram Update must contain one message",
        )
    })?;

    let chat_id = message.chat.id.to_string();
    let sender_id = message.from.id.to_string();
    if !is_telegram_id(&chat_id, true) || !is_telegram_id(&sender_id, false) {
        return Err(MkoError::new(
            "telegram_update_invalid",
            "Telegram chat or sender ID is invalid",
        ));
    }
    if !config.allowed_chat_ids.contains(&chat_id)
        || !config.allowed_sender_ids.contains(&sender_id)
    {
        return Err(MkoError::new(
            "telegram_identity_unauthorized",
            "Telegram chat or sender is not allowlisted for this profile",
        ));
    }

    let update_id = require_positive_telegram_id(update.update_id, "update_id")?;
    let message_id = require_positive_telegram_id(message.message_id, "message_id")?;
    let received_at = DateTime::from_timestamp(message.date, 0).ok_or_else(|| {
        MkoError::new(
            "telegram_update_invalid",
            "Telegram message date is outside the supported UTC range",
        )
    })?;

    if message.text.is_some() && message.caption.is_some() {
        return Err(MkoError::new(
            "telegram_input_mixed",
            "Telegram message cannot contain both text and caption inputs",
        ));
    }

    let (selected_scope, body) =
        command_and_body(message.text.as_deref().or(message.caption.as_deref()));
    let capture_input = match message.document {
        Some(document) => normalize_document(document, body)?,
        None => normalize_text_or_url(body)?,
    };

    let identity = ChannelIdentityV1 {
        profile_id: config.profile_id.clone(),
        chat_id,
        sender_id,
        update_id,
        message_id,
    };
    let capture_id = deterministic_capture_id(&identity, &capture_input, selected_scope.as_ref());
    let envelope = CaptureEnvelopeV1 {
        schema_version: 1,
        capture_id,
        channel: CaptureChannelV1::Telegram,
        channel_identity: identity,
        input: capture_input,
        selected_scope,
        received_at,
    };
    envelope.validate().map_err(|error| {
        MkoError::new(
            "telegram_envelope_invalid",
            format!("normalized Telegram input violates CaptureEnvelope v1: {error}"),
        )
    })?;
    Ok(envelope)
}

#[derive(Debug, Deserialize)]
struct TelegramUpdate {
    update_id: i64,
    #[serde(default)]
    message: Option<TelegramMessage>,
}

#[derive(Debug, Deserialize)]
struct TelegramMessage {
    message_id: i64,
    date: i64,
    chat: TelegramChat,
    from: TelegramUser,
    #[serde(default)]
    document: Option<TelegramDocument>,
    #[serde(default)]
    text: Option<String>,
    #[serde(default)]
    caption: Option<String>,
}

#[derive(Debug, Deserialize)]
struct TelegramChat {
    id: i64,
}

#[derive(Debug, Deserialize)]
struct TelegramUser {
    id: i64,
}

#[derive(Debug, Deserialize)]
struct TelegramDocument {
    file_id: String,
    file_unique_id: String,
    #[serde(default)]
    mime_type: Option<String>,
    #[serde(default)]
    file_size: Option<u64>,
    // Telegram's sender-controlled filename is intentionally never carried to
    // a CaptureEnvelope and is ignored by the normalizer.
    #[serde(rename = "file_name")]
    _file_name: Option<String>,
}

fn normalize_document(
    document: TelegramDocument,
    caption_body: Option<&str>,
) -> Result<CaptureInputV1, MkoError> {
    if caption_body.is_some_and(contains_url_like_input) {
        return Err(MkoError::new(
            "telegram_input_mixed",
            "a PDF document and URL cannot be captured in one message",
        ));
    }
    let mime_type = document.mime_type.unwrap_or_default();
    let declared_size_bytes = document.file_size.unwrap_or_default();
    if mime_type != "application/pdf"
        || declared_size_bytes == 0
        || declared_size_bytes > MAX_PDF_DECLARED_BYTES_V1
    {
        return Err(MkoError::new(
            "telegram_pdf_invalid",
            "Telegram document must declare application/pdf within the v1 size limit",
        ));
    }
    Ok(CaptureInputV1::TelegramPdf {
        file_id: document.file_id,
        file_unique_id: document.file_unique_id,
        declared_size_bytes,
        mime_type,
    })
}

fn normalize_text_or_url(body: Option<&str>) -> Result<CaptureInputV1, MkoError> {
    let Some(raw_url) = body.map(str::trim).filter(|value| !value.is_empty()) else {
        return Err(MkoError::new(
            "telegram_input_unsupported",
            "Telegram v1 capture accepts a PDF document or one YouTube URL",
        ));
    };
    if raw_url.split_whitespace().count() != 1 {
        return Err(MkoError::new(
            "telegram_input_mixed",
            "Telegram v1 capture accepts exactly one YouTube URL per message",
        ));
    }
    let (video_id, canonical_url) = canonicalize_youtube_url_v1(raw_url)?;
    Ok(CaptureInputV1::Youtube {
        video_id,
        canonical_url,
    })
}

/// Canonicalizes only the v1 allowlisted HTTPS YouTube forms:
///
/// - `https://youtube.com/watch?v=<id>` (or `www.youtube.com`)
/// - `https://youtu.be/<id>`
///
/// Query parameters are discarded after extracting exactly one `v` parameter.
/// Userinfo, explicit ports, non-HTTPS schemes, fragments, percent-encoding,
/// and all other host/path forms are rejected.
pub fn canonicalize_youtube_url_v1(raw_url: &str) -> Result<(String, String), MkoError> {
    if raw_url.len() > 2_048
        || raw_url.is_empty()
        || !raw_url.is_ascii()
        || raw_url.contains('#')
        || raw_url.contains('%')
        || raw_url.contains('\\')
        || !raw_url.starts_with("https://")
    {
        return invalid_youtube_url();
    }
    let remainder = &raw_url["https://".len()..];
    let (authority_and_path, query) = match remainder.split_once('?') {
        Some(parts) => parts,
        None => (remainder, ""),
    };
    let Some((host, path)) = authority_and_path.split_once('/') else {
        return invalid_youtube_url();
    };
    if host.is_empty() || host.contains('@') || host.contains(':') {
        return invalid_youtube_url();
    }

    let video_id = match host {
        "youtube.com" | "www.youtube.com" if path == "watch" => extract_youtube_watch_id(query)?,
        "youtu.be" if !path.contains('/') => {
            if path.is_empty() || !is_youtube_video_id(path) {
                return invalid_youtube_url();
            }
            path.to_owned()
        }
        _ => return invalid_youtube_url(),
    };
    Ok((
        video_id.clone(),
        format!("https://www.youtube.com/watch?v={video_id}"),
    ))
}

fn extract_youtube_watch_id(query: &str) -> Result<String, MkoError> {
    if query.is_empty() {
        return invalid_youtube_url();
    }
    let mut video_id = None;
    for pair in query.split('&') {
        let Some((key, value)) = pair.split_once('=') else {
            return invalid_youtube_url();
        };
        if key == "v" {
            if video_id.is_some() || !is_youtube_video_id(value) {
                return invalid_youtube_url();
            }
            video_id = Some(value.to_owned());
        }
    }
    video_id.ok_or_else(|| {
        MkoError::new(
            "telegram_youtube_invalid",
            "YouTube watch URL must have exactly one valid v parameter",
        )
    })
}

fn invalid_youtube_url<T>() -> Result<T, MkoError> {
    Err(MkoError::new(
        "telegram_youtube_invalid",
        "Telegram v1 accepts only allowlisted HTTPS YouTube URL forms",
    ))
}

fn command_and_body(input: Option<&str>) -> (Option<SubjectScopeV1>, Option<&str>) {
    let Some(input) = input else {
        return (None, None);
    };
    for (prefix, scope) in [
        ("/general", SubjectScopeV1::General),
        ("/finance", SubjectScopeV1::Finance),
    ] {
        if let Some(remainder) = input.strip_prefix(prefix)
            && (remainder.is_empty() || remainder.starts_with(char::is_whitespace))
        {
            return (Some(scope), Some(remainder.trim()));
        }
    }
    (None, Some(input))
}

fn contains_url_like_input(value: &str) -> bool {
    value
        .split_whitespace()
        .any(|token| token.starts_with("http://") || token.starts_with("https://"))
}

fn deterministic_capture_id(
    identity: &ChannelIdentityV1,
    input: &CaptureInputV1,
    selected_scope: Option<&SubjectScopeV1>,
) -> String {
    let mut material = format!(
        "mko-telegram-capture-v1\\0{}\\0{}\\0{}\\0{}\\0{}\\0",
        identity.profile_id,
        identity.chat_id,
        identity.sender_id,
        identity.update_id,
        identity.message_id,
    );
    match input {
        CaptureInputV1::TelegramPdf {
            file_id,
            file_unique_id,
            declared_size_bytes,
            mime_type,
        } => material.push_str(&format!(
            "telegram_pdf\\0{file_id}\\0{file_unique_id}\\0{declared_size_bytes}\\0{mime_type}"
        )),
        CaptureInputV1::Youtube {
            video_id,
            canonical_url,
        } => material.push_str(&format!("youtube\\0{video_id}\\0{canonical_url}")),
        CaptureInputV1::Text { text } => material.push_str(&format!("text\\0{text}")),
    }
    material.push('\0');
    material.push_str(match selected_scope {
        Some(SubjectScopeV1::General) => "general",
        Some(SubjectScopeV1::Finance) => "finance",
        None => "",
    });
    format!(
        "cap_tg_{}",
        hex::encode(Sha256::digest(material.as_bytes()))
    )
}

fn require_positive_telegram_id(value: i64, field: &str) -> Result<String, MkoError> {
    let value = value.to_string();
    if !is_telegram_id(&value, false) {
        return Err(MkoError::new(
            "telegram_update_invalid",
            format!("Telegram {field} must be a positive identifier"),
        ));
    }
    Ok(value)
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

fn is_youtube_video_id(value: &str) -> bool {
    value.len() == 11
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonicalizes_allowlisted_youtube_forms_and_discards_tracking() {
        assert_eq!(
            canonicalize_youtube_url_v1("https://youtube.com/watch?utm_source=test&v=dQw4w9WgXcQ")
                .unwrap(),
            (
                "dQw4w9WgXcQ".to_owned(),
                "https://www.youtube.com/watch?v=dQw4w9WgXcQ".to_owned(),
            )
        );
        assert_eq!(
            canonicalize_youtube_url_v1("https://youtu.be/dQw4w9WgXcQ?si=tracking").unwrap(),
            (
                "dQw4w9WgXcQ".to_owned(),
                "https://www.youtube.com/watch?v=dQw4w9WgXcQ".to_owned(),
            )
        );
    }

    #[test]
    fn rejects_untrusted_youtube_authority_and_path_forms() {
        for input in [
            "http://youtube.com/watch?v=dQw4w9WgXcQ",
            "https://user@youtube.com/watch?v=dQw4w9WgXcQ",
            "https://youtube.com:443/watch?v=dQw4w9WgXcQ",
            "https://youtube.com/shorts/dQw4w9WgXcQ",
            "https://youtu.be/dQw4w9WgXcQ/extra",
            "https://youtube.com.evil.example/watch?v=dQw4w9WgXcQ",
        ] {
            assert!(
                canonicalize_youtube_url_v1(input).is_err(),
                "must reject {input}"
            );
        }
    }
}
