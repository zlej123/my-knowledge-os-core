use std::{fs, io::Read, path::Path};

use sha2::{Digest, Sha256};

use crate::{error::MkoError, model::Fingerprint};

pub const MAX_ASSET_BYTES: u64 = 50 * 1024 * 1024;

pub fn fingerprint_file(path: &Path) -> Result<Fingerprint, MkoError> {
    let mut file = fs::File::open(path).map_err(io_error)?;
    let metadata = file.metadata().map_err(io_error)?;
    if metadata.len() > MAX_ASSET_BYTES {
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
