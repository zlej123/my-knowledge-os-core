use std::{
    io::{Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    thread,
    time::{Duration, Instant},
};

use cap_std::fs::File;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{
    error::MkoError,
    fingerprint::{FileSnapshot, MAX_ASSET_BYTES},
};

pub const EXTRACTOR_NAME: &str = "pdf-extract";
pub const EXTRACTOR_VERSION: &str = "0.12.0";
pub const MAX_PAGES: usize = 1_000;
pub const MAX_EXTRACTED_TEXT_BYTES: usize = 20 * 1024 * 1024;
pub const EXTRACTION_TIMEOUT: Duration = Duration::from_secs(120);
pub(crate) const STREAM_CHUNK_BYTES: usize = 64 * 1024;
const WORKER_JSON_OVERHEAD_BYTES: u64 = (MAX_PAGES as u64 * 3) + 4_096;
const MAX_WORKER_OUTPUT_BYTES: u64 = match (MAX_EXTRACTED_TEXT_BYTES as u64).checked_mul(6) {
    Some(escaped) => match escaped.checked_add(WORKER_JSON_OVERHEAD_BYTES) {
        Some(limit) => limit,
        None => panic!("worker output bound overflow"),
    },
    None => panic!("worker output bound overflow"),
};

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "result", rename_all = "snake_case")]
pub enum ExtractionWorkerResponse {
    Success { pages: Vec<String> },
    Error { code: String, message: String },
}

pub fn extract_pdf_pages(path: &Path) -> Result<Vec<String>, MkoError> {
    let file = std::fs::File::open(path)
        .map_err(|error| MkoError::new("pdf_extraction_failed", error.to_string()))?;
    extract_pdf_pages_from_reader(file)
}

pub fn extract_pdf_pages_from_reader(reader: impl Read) -> Result<Vec<String>, MkoError> {
    let mut bytes = Vec::new();
    reader
        .take(MAX_ASSET_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| MkoError::new("pdf_extraction_failed", error.to_string()))?;
    if bytes.len() as u64 > MAX_ASSET_BYTES {
        return Err(MkoError::new(
            "file_too_large",
            "PDF worker input exceeds the 50 MiB memory limit",
        ));
    }
    extract_pdf_pages_from_bytes(&bytes)
}

pub fn extract_pdf_pages_from_bytes(bytes: &[u8]) -> Result<Vec<String>, MkoError> {
    validate_pdf_page_limit_bytes(bytes)?;
    let pages = pdf_extract::extract_text_from_mem_by_pages(bytes)
        .map_err(|error| MkoError::new("pdf_extraction_failed", error.to_string()))?;
    validate_extracted_pages(&pages)?;
    Ok(pages)
}

pub fn validate_pdf_page_limit(path: &Path) -> Result<(), MkoError> {
    let file = std::fs::File::open(path)
        .map_err(|error| MkoError::new("pdf_extraction_failed", error.to_string()))?;
    let mut bytes = Vec::new();
    file.take(MAX_ASSET_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| MkoError::new("pdf_extraction_failed", error.to_string()))?;
    if bytes.len() as u64 > MAX_ASSET_BYTES {
        return Err(MkoError::new(
            "file_too_large",
            "PDF worker input exceeds the 50 MiB memory limit",
        ));
    }
    validate_pdf_page_limit_bytes(&bytes)
}

fn validate_pdf_page_limit_bytes(bytes: &[u8]) -> Result<(), MkoError> {
    let document = pdf_extract::Document::load_mem(bytes)
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
    snapshot: File,
    expected: &FileSnapshot,
) -> Result<Vec<String>, MkoError> {
    extract_pdf_pages_in_child_with_timeout(executable, snapshot, expected, EXTRACTION_TIMEOUT)
}

fn extract_pdf_pages_in_child_with_timeout(
    executable: &Path,
    snapshot: File,
    expected: &FileSnapshot,
    timeout: Duration,
) -> Result<Vec<String>, MkoError> {
    let mut command = Command::new(executable);
    command.arg("__extract-pdf");
    let output =
        run_child_with_timeout_and_input(&mut command, snapshot, expected, timeout, |_| {})?;
    decode_worker_output(output)
}

fn decode_worker_output(output: ChildOutput) -> Result<Vec<String>, MkoError> {
    if output.stdout.len() as u64 > MAX_WORKER_OUTPUT_BYTES {
        return Err(MkoError::new(
            "extracted_text_too_large",
            "extraction worker output exceeds the bounded response limit",
        ));
    }
    if !output.status.success() {
        return Err(MkoError::new(
            "pdf_extraction_failed",
            "PDF extraction worker failed",
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

#[cfg(test)]
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

fn run_child_with_timeout_and_input<F>(
    command: &mut Command,
    input: File,
    expected: &FileSnapshot,
    timeout: Duration,
    after_chunk: F,
) -> Result<ChildOutput, MkoError>
where
    F: FnMut(usize) + Send + 'static,
{
    let mut child = command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|error| MkoError::new("pdf_extraction_failed", error.to_string()))?;
    let stdin = child.stdin.take().ok_or_else(|| {
        MkoError::new(
            "pdf_extraction_failed",
            "extraction worker stdin was unavailable",
        )
    })?;
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
    let expected = expected.clone();
    let input_writer = thread::spawn(move || stream_snapshot(input, stdin, &expected, after_chunk));
    let status = wait_for_child(&mut child, timeout);
    let output = output_reader
        .join()
        .map_err(|_| MkoError::new("pdf_extraction_failed", "worker output reader panicked"))?
        .map_err(|error| MkoError::new("pdf_extraction_failed", error.to_string()));
    let input = input_writer
        .join()
        .map_err(|_| MkoError::new("pdf_extraction_failed", "worker input writer panicked"))?;
    let status = status?;
    let output = output?;
    input?;
    Ok(ChildOutput {
        status,
        stdout: output,
    })
}

fn wait_for_child(
    child: &mut Child,
    timeout: Duration,
) -> Result<std::process::ExitStatus, MkoError> {
    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return Ok(status),
            Ok(None) if Instant::now() < deadline => thread::sleep(Duration::from_millis(10)),
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(MkoError::new(
                    "extraction_timeout",
                    "PDF extraction exceeded 120 seconds",
                ));
            }
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(MkoError::new("pdf_extraction_failed", error.to_string()));
            }
        }
    }
}

fn stream_snapshot<F>(
    mut snapshot: File,
    mut stdin: std::process::ChildStdin,
    expected: &FileSnapshot,
    mut after_chunk: F,
) -> Result<(), MkoError>
where
    F: FnMut(usize),
{
    snapshot
        .seek(SeekFrom::Start(0))
        .map_err(|error| MkoError::new("file_unreadable", error.to_string()))?;
    let mut hasher = Sha256::new();
    let mut size_bytes = 0_u64;
    let mut buffer = [0_u8; STREAM_CHUNK_BYTES];
    let mut chunk = 0_usize;
    loop {
        let read = snapshot
            .read(&mut buffer)
            .map_err(|error| MkoError::new("file_unreadable", error.to_string()))?;
        if read == 0 {
            break;
        }
        size_bytes = size_bytes
            .checked_add(read as u64)
            .ok_or_else(|| MkoError::new("file_too_large", "PDF stream size overflow"))?;
        if size_bytes > MAX_ASSET_BYTES {
            return Err(MkoError::new(
                "file_too_large",
                "PDF stream exceeds the 50 MiB limit",
            ));
        }
        hasher.update(&buffer[..read]);
        stdin
            .write_all(&buffer[..read])
            .map_err(|error| MkoError::new("pdf_extraction_failed", error.to_string()))?;
        chunk += 1;
        after_chunk(chunk);
    }
    drop(stdin);
    let fingerprint = format!("sha256:{}", hex::encode(hasher.finalize()));
    if size_bytes != expected.size_bytes || fingerprint != expected.fingerprint.value {
        return Err(MkoError::new(
            "fingerprint_changed",
            "runtime PDF snapshot changed while streaming to the extraction worker",
        ));
    }
    Ok(())
}

pub fn worker_executable() -> Result<PathBuf, MkoError> {
    std::env::current_exe()
        .map_err(|error| MkoError::new("pdf_extraction_failed", error.to_string()))
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        io::{Read, Seek, SeekFrom, Write},
        process::Command,
        time::Duration,
    };

    use cap_std::{ambient_authority, fs::Dir};

    use super::{
        MAX_WORKER_OUTPUT_BYTES, STREAM_CHUNK_BYTES, decode_worker_output, run_child_with_timeout,
        run_child_with_timeout_and_input,
    };
    use crate::fingerprint::fingerprint_open_file;

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

    #[test]
    fn oversized_worker_output_wins_over_a_failed_exit_status() {
        let executable = std::env::current_exe().unwrap();
        let mut command = Command::new(executable);
        command
            .args([
                "--exact",
                "pdf::tests::oversized_failed_child",
                "--nocapture",
            ])
            .env("MKO_TEST_OVERSIZED_CHILD", "1");

        let output = run_child_with_timeout(&mut command, Duration::from_secs(5)).unwrap();
        let error = decode_worker_output(output).unwrap_err();

        assert_eq!(error.code(), "extracted_text_too_large");
    }

    #[test]
    fn oversized_failed_child() {
        if std::env::var_os("MKO_TEST_OVERSIZED_CHILD").is_some() {
            let block = vec![b'x'; 64 * 1024];
            let mut stdout = std::io::stdout().lock();
            for _ in 0..=((MAX_WORKER_OUTPUT_BYTES / block.len() as u64) + 1) {
                if stdout.write_all(&block).is_err() {
                    break;
                }
            }
            let _ = stdout.flush();
            std::process::exit(7);
        }
    }

    #[test]
    fn worker_transport_accepts_twenty_mib_of_worst_case_json_escaped_text() {
        let pages = vec!["\u{0001}".repeat(super::MAX_EXTRACTED_TEXT_BYTES)];
        let stdout =
            serde_json::to_vec(&super::ExtractionWorkerResponse::Success { pages }).unwrap();
        let status = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "pdf::tests::successful_child"])
            .status()
            .unwrap();

        let decoded = decode_worker_output(super::ChildOutput { status, stdout }).unwrap();

        assert_eq!(decoded[0].len(), super::MAX_EXTRACTED_TEXT_BYTES);
    }

    #[test]
    fn successful_child() {}

    #[cfg(unix)]
    #[test]
    fn retained_snapshot_handle_ignores_path_replacement_during_stream() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("snapshot.pdf");
        let retained_path = root.path().join("retained.pdf");
        let original = vec![b'a'; STREAM_CHUNK_BYTES * 3];
        fs::write(&path, &original).unwrap();
        let directory = Dir::open_ambient_dir(root.path(), ambient_authority()).unwrap();
        let mut snapshot = directory.open("snapshot.pdf").unwrap();
        let expected = fingerprint_open_file(&mut snapshot).unwrap();
        let executable = std::env::current_exe().unwrap();
        let mut command = Command::new(executable);
        command
            .args(["--exact", "pdf::tests::streaming_child", "--nocapture"])
            .env("MKO_TEST_STREAMING_CHILD", "1");
        let path_for_hook = path.clone();

        let output = run_child_with_timeout_and_input(
            &mut command,
            snapshot,
            &expected,
            Duration::from_secs(5),
            move |chunk| {
                if chunk == 1 {
                    fs::rename(&path_for_hook, &retained_path).unwrap();
                    fs::write(&path_for_hook, b"replacement path bytes").unwrap();
                    fs::remove_file(&path_for_hook).unwrap();
                    fs::rename(&retained_path, &path_for_hook).unwrap();
                }
            },
        )
        .unwrap();

        assert!(output.status.success());
        assert_eq!(fs::read(path).unwrap(), original);
    }

    #[test]
    fn in_place_mutation_during_snapshot_stream_is_rejected() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("snapshot.pdf");
        fs::write(&path, vec![b'a'; STREAM_CHUNK_BYTES * 4]).unwrap();
        let directory = Dir::open_ambient_dir(root.path(), ambient_authority()).unwrap();
        let mut snapshot = directory.open("snapshot.pdf").unwrap();
        let expected = fingerprint_open_file(&mut snapshot).unwrap();
        let executable = std::env::current_exe().unwrap();
        let mut command = Command::new(executable);
        command
            .args(["--exact", "pdf::tests::streaming_child", "--nocapture"])
            .env("MKO_TEST_STREAMING_CHILD", "1");
        let path_for_hook = path.clone();

        let error = run_child_with_timeout_and_input(
            &mut command,
            snapshot,
            &expected,
            Duration::from_secs(5),
            move |chunk| {
                if chunk == 1 {
                    let mut source = fs::OpenOptions::new()
                        .write(true)
                        .open(&path_for_hook)
                        .unwrap();
                    source
                        .seek(SeekFrom::Start((STREAM_CHUNK_BYTES * 2) as u64))
                        .unwrap();
                    source.write_all(&vec![b'b'; STREAM_CHUNK_BYTES]).unwrap();
                    source.sync_all().unwrap();
                }
            },
        )
        .unwrap_err();

        assert_eq!(error.code(), "fingerprint_changed");
    }

    #[test]
    fn streaming_child() {
        if std::env::var_os("MKO_TEST_STREAMING_CHILD").is_some() {
            let mut bytes = Vec::new();
            std::io::stdin().read_to_end(&mut bytes).unwrap();
        }
    }
}
