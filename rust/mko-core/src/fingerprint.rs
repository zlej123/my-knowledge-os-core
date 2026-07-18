use std::{
    fs,
    io::{Read, Seek, SeekFrom},
    path::Path,
};

use cap_std::{fs::File, time::SystemTime};
use sha2::{Digest, Sha256};

use crate::{error::MkoError, model::Fingerprint};

pub const MAX_ASSET_BYTES: u64 = 50 * 1024 * 1024;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FileSnapshot {
    pub fingerprint: Fingerprint,
    pub size_bytes: u64,
    pub modified_at: SystemTime,
}

pub fn fingerprint_file(path: &Path) -> Result<Fingerprint, MkoError> {
    let mut file = fs::File::open(path).map_err(io_error)?;
    let metadata = file.metadata().map_err(io_error)?;
    fingerprint_reader(&mut file, metadata.len())
}

pub fn validate_pdf_content(file: &mut File) -> Result<(), MkoError> {
    let mut signature = [0_u8; 5];
    file.seek(SeekFrom::Start(0)).map_err(io_error)?;
    let valid = file.read_exact(&mut signature).is_ok() && signature == *b"%PDF-";
    file.seek(SeekFrom::Start(0)).map_err(io_error)?;
    if !valid {
        return Err(MkoError::new(
            "invalid_pdf",
            "file does not have a PDF signature",
        ));
    }
    Ok(())
}

pub fn fingerprint_open_file(file: &mut File) -> Result<FileSnapshot, MkoError> {
    let metadata = file.metadata().map_err(io_error)?;
    file.seek(SeekFrom::Start(0)).map_err(io_error)?;
    let fingerprint = fingerprint_reader(file, metadata.len())?;
    file.seek(SeekFrom::Start(0)).map_err(io_error)?;
    let modified_at = metadata.modified().map_err(io_error)?;
    Ok(FileSnapshot {
        fingerprint,
        size_bytes: metadata.len(),
        modified_at,
    })
}

fn fingerprint_reader(file: &mut impl Read, size_bytes: u64) -> Result<Fingerprint, MkoError> {
    if size_bytes > MAX_ASSET_BYTES {
        return Err(MkoError::new(
            "file_too_large",
            "PDF exceeds 50 MiB; use the documented manual processing path",
        ));
    }

    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer).map_err(io_error)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(Fingerprint {
        method: "sha256".into(),
        value: format!("sha256:{}", hex::encode(hasher.finalize())),
    })
}

pub fn asset_id(fingerprint: &Fingerprint) -> Result<String, MkoError> {
    let Some(hash) = fingerprint.value.strip_prefix("sha256:") else {
        return Err(MkoError::new(
            "fingerprint_invalid",
            "fingerprint must use sha256",
        ));
    };
    if fingerprint.method != "sha256"
        || hash.len() != 64
        || !hash
            .bytes()
            .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
    {
        return Err(MkoError::new(
            "fingerprint_invalid",
            "fingerprint must be lowercase SHA-256",
        ));
    }
    Ok(format!("personal-asset-{hash}"))
}

fn io_error(error: std::io::Error) -> MkoError {
    MkoError::new("file_unreadable", error.to_string())
}
