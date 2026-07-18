use std::{
    io::Read,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

use serde::{Deserialize, Serialize};

use crate::error::MkoError;

pub const EXTRACTOR_NAME: &str = "pdf-extract";
pub const EXTRACTOR_VERSION: &str = "0.12.0";
pub const MAX_PAGES: usize = 1_000;
pub const MAX_EXTRACTED_TEXT_BYTES: usize = 20 * 1024 * 1024;
pub const EXTRACTION_TIMEOUT: Duration = Duration::from_secs(120);
const MAX_WORKER_OUTPUT_BYTES: u64 = (MAX_EXTRACTED_TEXT_BYTES as u64) + (2 * 1024 * 1024);

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "result", rename_all = "snake_case")]
pub enum ExtractionWorkerResponse {
    Success { pages: Vec<String> },
    Error { code: String, message: String },
}

pub fn extract_pdf_pages(path: &Path) -> Result<Vec<String>, MkoError> {
    validate_pdf_page_limit(path)?;
    let pages = pdf_extract::extract_text_by_pages(path)
        .map_err(|error| MkoError::new("pdf_extraction_failed", error.to_string()))?;
    validate_extracted_pages(&pages)?;
    Ok(pages)
}

pub fn validate_pdf_page_limit(path: &Path) -> Result<(), MkoError> {
    let document = pdf_extract::Document::load(path)
        .map_err(|error| MkoError::new("pdf_extraction_failed", error.to_string()))?;
    if document.get_pages().len() > MAX_PAGES {
        return Err(MkoError::new(
            "page_limit_exceeded",
            "PDF exceeds the 1,000 page extraction limit",
        ));
    }
    Ok(())
}

pub fn validate_extracted_pages(pages: &[String]) -> Result<(), MkoError> {
    if pages.len() > MAX_PAGES {
        return Err(MkoError::new(
            "page_limit_exceeded",
            "PDF exceeds the 1,000 page extraction limit",
        ));
    }
    let text_bytes = pages.iter().try_fold(0_usize, |total, page| {
        total.checked_add(page.len()).ok_or_else(|| {
            MkoError::new(
                "extracted_text_too_large",
                "extracted PDF text exceeds the 20 MiB limit",
            )
        })
    })?;
    if text_bytes > MAX_EXTRACTED_TEXT_BYTES {
        return Err(MkoError::new(
            "extracted_text_too_large",
            "extracted PDF text exceeds the 20 MiB limit",
        ));
    }
    Ok(())
}

pub fn extract_pdf_pages_in_child(
    executable: &Path,
    snapshot: &Path,
) -> Result<Vec<String>, MkoError> {
    extract_pdf_pages_in_child_with_timeout(executable, snapshot, EXTRACTION_TIMEOUT)
}

fn extract_pdf_pages_in_child_with_timeout(
    executable: &Path,
    snapshot: &Path,
    timeout: Duration,
) -> Result<Vec<String>, MkoError> {
    let mut command = Command::new(executable);
    command.arg("__extract-pdf").arg("--file").arg(snapshot);
    let output = run_child_with_timeout(&mut command, timeout)?;
    if !output.status.success() {
        return Err(MkoError::new(
            "pdf_extraction_failed",
            "PDF extraction worker failed",
        ));
    }
    if output.stdout.len() as u64 > MAX_WORKER_OUTPUT_BYTES {
        return Err(MkoError::new(
            "extracted_text_too_large",
            "extraction worker output exceeds the bounded response limit",
        ));
    }
    let output: ExtractionWorkerResponse = serde_json::from_slice(&output.stdout)
        .map_err(|error| MkoError::new("pdf_extraction_failed", error.to_string()))?;
    match output {
        ExtractionWorkerResponse::Success { pages } => {
            validate_extracted_pages(&pages)?;
            Ok(pages)
        }
        ExtractionWorkerResponse::Error { code, message } => Err(MkoError::new(code, message)),
    }
}

#[derive(Debug)]
struct ChildOutput {
    status: std::process::ExitStatus,
    stdout: Vec<u8>,
}

fn run_child_with_timeout(
    command: &mut Command,
    timeout: Duration,
) -> Result<ChildOutput, MkoError> {
    let mut child = command
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|error| MkoError::new("pdf_extraction_failed", error.to_string()))?;
    let stdout = child.stdout.take().ok_or_else(|| {
        MkoError::new(
            "pdf_extraction_failed",
            "extraction worker stdout was unavailable",
        )
    })?;
    let output_reader = thread::spawn(move || {
        let mut output = Vec::new();
        stdout
            .take(MAX_WORKER_OUTPUT_BYTES + 1)
            .read_to_end(&mut output)
            .map(|_| output)
    });
    let deadline = Instant::now() + timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() < deadline => thread::sleep(Duration::from_millis(10)),
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                let _ = output_reader.join();
                return Err(MkoError::new(
                    "extraction_timeout",
                    "PDF extraction exceeded 120 seconds",
                ));
            }
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                let _ = output_reader.join();
                return Err(MkoError::new("pdf_extraction_failed", error.to_string()));
            }
        }
    };
    let output = output_reader
        .join()
        .map_err(|_| MkoError::new("pdf_extraction_failed", "worker output reader panicked"))?
        .map_err(|error| MkoError::new("pdf_extraction_failed", error.to_string()))?;
    Ok(ChildOutput {
        status,
        stdout: output,
    })
}

pub fn worker_executable() -> Result<PathBuf, MkoError> {
    std::env::current_exe()
        .map_err(|error| MkoError::new("pdf_extraction_failed", error.to_string()))
}

#[cfg(test)]
mod tests {
    use std::{process::Command, time::Duration};

    use super::run_child_with_timeout;

    #[test]
    fn timeout_terminates_the_extraction_child() {
        let executable = std::env::current_exe().unwrap();
        let mut command = Command::new(executable);
        command
            .args(["--exact", "pdf::tests::sleeping_child", "--nocapture"])
            .env("MKO_TEST_SLEEPING_CHILD", "1");

        let error = run_child_with_timeout(&mut command, Duration::from_millis(25)).unwrap_err();

        assert_eq!(error.code(), "extraction_timeout");
    }

    #[test]
    fn sleeping_child() {
        if std::env::var_os("MKO_TEST_SLEEPING_CHILD").is_some() {
            std::thread::sleep(Duration::from_secs(10));
        }
    }
}
