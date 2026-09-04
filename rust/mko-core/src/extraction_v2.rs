//! Agent-read text for an original that carries no text of its own — OCR
//! output for an image, a converted document body — kept as immutable
//! evidence in the knowledge base (§6, §6.2; owner decision 2026-09-03).
//!
//! Before this store existed, the text an agent supplied at prepare time
//! lived only in the prepared bundle under `.mko/runtime` — gitignored and
//! gone after 24 hours. A Source revision's evidence basis and every
//! per-claim evidence locator then pointed at text that no longer existed
//! anywhere, and two OCR passes over the same original could never be
//! compared. So the supplied text is stored here, content-addressed at
//! `assets/extractions/<sha256-of-text>.txt`, and the revision carries the
//! digest (`EvidenceBasisV2::extraction_digest`). Re-extraction with
//! different text writes a second file; the first stays, because the
//! earlier revision's evidence must remain resolvable.
//!
//! The same digest field also names text that is *already* stored for the
//! other origins — a snapshot, a paste, a conversation, a video transcript,
//! or a text local file — whose text is the Asset's own identity and lives in
//! `assets/snapshots/` or `assets/originals/`. `read_extraction_text_v2`
//! resolves a digest across all of those stores, so a caller holding a
//! revision never needs to know which origin produced it.
//!
//! The extractor label tells the rest of the truth: every non-PDF bundle is
//! stamped `agent-read` at the product version that accepted the text, never
//! the PDF extractor's name.

use std::{
    fs,
    path::{Path, PathBuf},
};

use crate::{
    asset_v2::read_bounded_nofollow,
    atomic::{write_new, write_replace},
    error::MkoError,
    revision_v2::sha256_digest,
    version::PRODUCT_VERSION,
};

/// The same ceiling a snapshot or a text original has (§8): generous for a
/// page of OCR or a converted document, bounded so a runaway extraction
/// cannot fill the knowledge base. Refused, never truncated.
pub const MAX_EXTRACTION_BYTES: u64 = 2 * 1024 * 1024;

/// What every non-PDF prepared bundle names as its extractor (§6.2): the
/// agent read the text — pasted it, fetched it, transcribed it, OCR'd it,
/// converted it — and the Core only accepted it.
pub const AGENT_READ_EXTRACTOR_NAME: &str = "agent-read";

/// The product version that accepted the text, so a later reader can tell
/// which contract the label was written under.
pub const AGENT_READ_EXTRACTOR_VERSION: &str = PRODUCT_VERSION;

const EXTRACTIONS_DIRECTORY: &str = "assets/extractions";
const SNAPSHOTS_DIRECTORY: &str = "assets/snapshots";
const ORIGINALS_DIRECTORY: &str = "assets/originals";
/// Text originals are filed under the extension of the locator they were
/// registered from (`local_file_v2`); these are the text-kind extensions.
const TEXT_ORIGINAL_EXTENSIONS: &[&str] = &["md", "markdown", "txt"];

/// Validates supplied agent-read text and returns the digest it will be
/// stored under, without writing anything. The prepared bundle carries this
/// digest, so it must be known before the bundle is built and before any
/// lock is held.
pub fn extraction_digest_v2(text: &str) -> Result<String, MkoError> {
    if text.len() as u64 > MAX_EXTRACTION_BYTES {
        return Err(MkoError::new(
            "extraction_too_large",
            "the supplied extracted text is larger than an extraction may be",
        ));
    }
    if text.trim().is_empty() {
        return Err(MkoError::new(
            "local_file_extracted_text_empty",
            "supplied extracted text has no readable content",
        ));
    }
    Ok(sha256_digest(text.as_bytes()))
}

/// Stores supplied agent-read text content-addressed under
/// `assets/extractions/`, returning its digest. Idempotent for identical
/// text; a stored file whose bytes disagree with its own name is provably
/// damaged and is repaired in place, mirroring the snapshot and originals
/// stores.
pub fn write_extraction_text_v2(repository_root: &Path, text: &str) -> Result<String, MkoError> {
    let digest = extraction_digest_v2(text)?;
    let hash = hex_of(&digest)?;
    let bytes = text.as_bytes();
    // Knowledge bases scaffolded before this store existed have no such
    // directory, and evidence should not need a migration to start being
    // stored: create it on first write, refusing anything that is not a
    // real directory. Mirrors `snapshot_v2::write_snapshot_text`.
    let directory = repository_root.join(EXTRACTIONS_DIRECTORY);
    match fs::create_dir(&directory) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            let metadata = fs::symlink_metadata(&directory)
                .map_err(|error| MkoError::new("extraction_write_failed", error.to_string()))?;
            if !metadata.file_type().is_dir() || metadata.file_type().is_symlink() {
                return Err(MkoError::new(
                    "extraction_destination_invalid",
                    "assets/extractions must be a real directory",
                ));
            }
        }
        Err(error) => return Err(MkoError::new("extraction_write_failed", error.to_string())),
    }
    let path = extraction_path(repository_root, hash);
    let outcome = write_new(&path, bytes, |existing| {
        let stored = read_bounded_nofollow(existing, MAX_EXTRACTION_BYTES, "extraction")?;
        if stored == bytes {
            Ok(())
        } else {
            Err(MkoError::new(
                "extraction_damaged",
                "stored extracted text does not match the identity it is filed under",
            ))
        }
    });
    match outcome {
        Ok(_) => Ok(digest),
        // The hash is the identity: bytes that disagree with it are provably
        // damaged, and these bytes are provably what belongs there.
        Err(error) if error.code() == "extraction_damaged" => {
            write_replace(&path, bytes).map(|()| digest)
        }
        Err(error) => Err(error),
    }
}

/// Where the text behind a digest lives in the knowledge base, as a
/// repository-relative path, or `None` when no store holds it.
///
/// Looks in `assets/extractions/` first (agent-read text for an image or
/// document original), then the snapshot store (web, paste, conversation,
/// video), then the text originals store (md/txt local files) — every place
/// a prepared bundle's text can come from. Presence only: whether the bytes
/// still hash to the name is `read_extraction_text_v2`'s job.
pub fn locate_extraction_text_v2(
    repository_root: &Path,
    digest: &str,
) -> Result<Option<String>, MkoError> {
    let hash = hex_of(digest)?;
    for relative in candidate_relative_paths(hash) {
        match fs::symlink_metadata(repository_root.join(&relative)) {
            Ok(metadata) if metadata.file_type().is_file() => return Ok(Some(relative)),
            Ok(_) => {
                return Err(MkoError::new(
                    "extraction_invalid",
                    format!("{relative} must be a regular non-link file"),
                ));
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(MkoError::new("extraction_unreadable", error.to_string())),
        }
    }
    Ok(None)
}

/// The text a revision's evidence was built from, read back from whichever
/// store holds it and verified against the digest that names it — so what
/// comes back is provably the text the agent handed over, not merely a file
/// that happens to sit at that name.
pub fn read_extraction_text_v2(repository_root: &Path, digest: &str) -> Result<String, MkoError> {
    let relative = locate_extraction_text_v2(repository_root, digest)?.ok_or_else(|| {
        MkoError::new(
            "extraction_not_found",
            format!("no stored text in the knowledge base has digest {digest}"),
        )
    })?;
    let bytes = read_bounded_nofollow(
        &repository_root.join(&relative),
        MAX_EXTRACTION_BYTES,
        "extraction",
    )?;
    if sha256_digest(&bytes) != digest {
        return Err(MkoError::new(
            "extraction_damaged",
            format!("{relative} no longer matches the digest its name claims"),
        ));
    }
    String::from_utf8(bytes).map_err(|error| MkoError::new("extraction_invalid", error.to_string()))
}

fn candidate_relative_paths(hash: &str) -> Vec<String> {
    let mut candidates = vec![
        format!("{EXTRACTIONS_DIRECTORY}/{hash}.txt"),
        format!("{SNAPSHOTS_DIRECTORY}/{hash}.txt"),
    ];
    candidates.extend(
        TEXT_ORIGINAL_EXTENSIONS
            .iter()
            .map(|extension| format!("{ORIGINALS_DIRECTORY}/{hash}.{extension}")),
    );
    candidates
}

fn extraction_path(repository_root: &Path, hash: &str) -> PathBuf {
    repository_root
        .join(EXTRACTIONS_DIRECTORY)
        .join(format!("{hash}.txt"))
}

/// The 64 lowercase hex characters of a canonical `sha256:` digest. A digest
/// in any other form names nothing and must not be turned into a path.
fn hex_of(digest: &str) -> Result<&str, MkoError> {
    digest
        .strip_prefix("sha256:")
        .filter(|hex| {
            hex.len() == 64
                && hex
                    .bytes()
                    .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
        })
        .ok_or_else(|| {
            MkoError::new(
                "extraction_digest_invalid",
                "an extraction digest must be a lowercase sha256: digest",
            )
        })
}

#[cfg(test)]
mod tests {
    use super::{
        AGENT_READ_EXTRACTOR_NAME, AGENT_READ_EXTRACTOR_VERSION, extraction_digest_v2, hex_of,
    };
    use crate::version::PRODUCT_VERSION;

    #[test]
    fn the_agent_read_label_is_pinned_to_the_product_version() {
        assert_eq!(AGENT_READ_EXTRACTOR_NAME, "agent-read");
        assert_eq!(AGENT_READ_EXTRACTOR_VERSION, PRODUCT_VERSION);
    }

    #[test]
    fn only_canonical_digests_name_a_stored_extraction() {
        assert!(hex_of(&format!("sha256:{}", "a".repeat(64))).is_ok());
        for bad in [
            "",
            "sha256:",
            &format!("sha256:{}", "A".repeat(64)),
            &format!("sha256:{}", "a".repeat(63)),
            &format!("md5:{}", "a".repeat(64)),
            "sha256:../../knowledge-os.yaml",
        ] {
            assert_eq!(hex_of(bad).unwrap_err().code(), "extraction_digest_invalid");
        }
    }

    #[test]
    fn empty_or_oversized_text_is_refused_before_anything_is_stored() {
        assert_eq!(
            extraction_digest_v2("  \n\t ").unwrap_err().code(),
            "local_file_extracted_text_empty"
        );
        let oversized = "x".repeat(2 * 1024 * 1024 + 1);
        assert_eq!(
            extraction_digest_v2(&oversized).unwrap_err().code(),
            "extraction_too_large"
        );
    }
}
