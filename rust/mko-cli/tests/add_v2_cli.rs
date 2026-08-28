use std::fs;

use assert_cmd::Command;
use mko_core::scaffold_v2::scaffold_personal_kb_v2;
use tempfile::tempdir;

#[test]
#[allow(deprecated)]
fn add_registers_an_inbox_pdf_and_reuses_its_content_identity() {
    let root = tempdir().unwrap();
    let repository = root.path().join("kb");
    let provider = root.path().join("Personal Inbox");
    scaffold_personal_kb_v2(&repository).unwrap();
    fs::create_dir_all(provider.join("papers")).unwrap();
    let pdf = provider.join("papers/example.pdf");
    fs::write(&pdf, b"%PDF-1.7\nfixture").unwrap();

    let first = Command::cargo_bin("mko")
        .unwrap()
        .arg("add")
        .arg(&pdf)
        .args(["--format", "json-v2"])
        .env("MKO_PERSONAL_PROVIDER_ROOT", &provider)
        .current_dir(&repository)
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let first: serde_json::Value = serde_json::from_slice(&first).unwrap();
    assert_eq!(first["command"], "add");
    assert_eq!(first["data"]["outcome"], "created");
    assert_eq!(first["data"]["logical_locator"], "papers/example.pdf");
    assert!(
        repository
            .join("assets/registry")
            .read_dir()
            .unwrap()
            .count()
            == 1
    );

    let second = Command::cargo_bin("mko")
        .unwrap()
        .arg("add")
        .arg("papers/example.pdf")
        .args(["--format", "json-v2"])
        .env("MKO_PERSONAL_PROVIDER_ROOT", &provider)
        .current_dir(&repository)
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let second: serde_json::Value = serde_json::from_slice(&second).unwrap();
    assert_eq!(second["data"]["outcome"], "existing");
    assert_eq!(second["data"]["asset_id"], first["data"]["asset_id"]);
}

#[test]
#[allow(deprecated)]
fn add_rejects_an_outside_pdf_with_a_typed_recovery() {
    let root = tempdir().unwrap();
    let repository = root.path().join("kb");
    let provider = root.path().join("Personal Inbox");
    scaffold_personal_kb_v2(&repository).unwrap();
    fs::create_dir_all(&provider).unwrap();
    let outside = root.path().join("outside.pdf");
    fs::write(&outside, b"%PDF-1.7\noutside").unwrap();

    let output = Command::cargo_bin("mko")
        .unwrap()
        .arg("add")
        .arg(&outside)
        .args(["--format", "json-v2"])
        .env("MKO_PERSONAL_PROVIDER_ROOT", &provider)
        .current_dir(&repository)
        .assert()
        .code(1)
        .get_output()
        .stdout
        .clone();
    let output: serde_json::Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(output["command"], "add");
    assert_eq!(output["error"]["code"], "asset_outside_inbox");
    assert_eq!(output["error"]["next_action"], "add");
    assert!(
        repository
            .join("assets/registry")
            .read_dir()
            .unwrap()
            .next()
            .is_none()
    );
}

#[test]
#[allow(deprecated)]
fn add_inbox_returns_partial_success_without_hiding_blocked_items() {
    let root = tempdir().unwrap();
    let repository = root.path().join("kb");
    let provider = root.path().join("Personal Inbox");
    scaffold_personal_kb_v2(&repository).unwrap();
    fs::create_dir_all(provider.join("papers")).unwrap();
    fs::write(provider.join("papers/a.pdf"), b"%PDF-1.7\nfirst").unwrap();
    fs::write(provider.join("papers/b.pdf"), b"not a pdf").unwrap();

    let output = Command::cargo_bin("mko")
        .unwrap()
        .args(["add", "--inbox", "--format", "json-v2"])
        .env("MKO_PERSONAL_PROVIDER_ROOT", &provider)
        .current_dir(&repository)
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let output: serde_json::Value = serde_json::from_slice(&output).unwrap();

    assert_eq!(output["command"], "add");
    assert_eq!(output["data"]["scan_complete"], true);
    assert_eq!(output["data"]["remaining"], 0);
    assert_eq!(output["data"]["items"].as_array().unwrap().len(), 2);
    assert_eq!(
        output["data"]["items"][0]["logical_locator"],
        "papers/a.pdf"
    );
    assert_eq!(output["data"]["items"][0]["outcome"], "created");
    assert!(output["data"]["items"][0]["error"].is_null());
    assert_eq!(
        output["data"]["items"][1]["logical_locator"],
        "papers/b.pdf"
    );
    assert!(output["data"]["items"][1]["asset_id"].is_null());
    assert_eq!(output["data"]["items"][1]["error"]["code"], "invalid_pdf");

    let second = Command::cargo_bin("mko")
        .unwrap()
        .args(["add", "--inbox", "--format", "json-v2"])
        .env("MKO_PERSONAL_PROVIDER_ROOT", &provider)
        .current_dir(&repository)
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let second: serde_json::Value = serde_json::from_slice(&second).unwrap();
    assert_eq!(second["data"]["items"][0]["outcome"], "existing");
    assert_eq!(
        second["data"]["items"][0]["asset_id"],
        output["data"]["items"][0]["asset_id"]
    );
}

// The agent fetches and hands Core the text; Core does the deterministic part.
// The text arrives in a file because a page body does not belong on a command
// line — it would reach process listings and shell history.
#[test]
#[allow(deprecated)]
fn a_page_the_agent_read_becomes_registered_evidence() {
    let root = tempdir().unwrap();
    let repository = root.path().join("kb");
    let provider = root.path().join("Personal Inbox");
    scaffold_personal_kb_v2(&repository).unwrap();
    fs::create_dir_all(&provider).unwrap();
    let text = root.path().join("page.txt");
    fs::write(&text, "The page said this.").unwrap();

    let output = Command::cargo_bin("mko")
        .unwrap()
        .args(["add", "--snapshot"])
        .arg(&text)
        .args([
            "--url",
            "https://example.com/page",
            "--title",
            "Example page",
            "--format",
            "json-v2",
        ])
        .env("MKO_PERSONAL_PROVIDER_ROOT", &provider)
        .current_dir(&repository)
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let report: serde_json::Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(report["command"], "add");
    assert_eq!(report["data"]["outcome"], "created");
    assert_eq!(
        report["data"]["logical_locator"],
        "https://example.com/page"
    );
    assert!(
        report["data"]["asset_id"]
            .as_str()
            .unwrap()
            .starts_with("personal-asset-")
    );
}

// A page that rendered nothing readable must say so. Registering an Asset
// holding whitespace would put material in the waiting list that no session
// could ever draft.
#[test]
#[allow(deprecated)]
fn a_page_with_no_readable_text_reports_why_instead_of_registering() {
    let root = tempdir().unwrap();
    let repository = root.path().join("kb");
    let provider = root.path().join("Personal Inbox");
    scaffold_personal_kb_v2(&repository).unwrap();
    fs::create_dir_all(&provider).unwrap();
    let text = root.path().join("empty.txt");
    fs::write(&text, "   \n\t ").unwrap();

    let output = Command::cargo_bin("mko")
        .unwrap()
        .args(["add", "--snapshot"])
        .arg(&text)
        .args([
            "--url",
            "https://example.com/js-only",
            "--title",
            "JavaScript page",
            "--format",
            "json-v2",
        ])
        .env("MKO_PERSONAL_PROVIDER_ROOT", &provider)
        .current_dir(&repository)
        .assert()
        .code(1)
        .get_output()
        .stdout
        .clone();

    let report: serde_json::Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(report["error"]["code"], "snapshot_text_empty");
    assert_eq!(report["error"]["retryable"], false);
    assert_eq!(
        repository
            .join("assets/registry")
            .read_dir()
            .unwrap()
            .count(),
        0,
        "nothing may be registered for a page that produced no text"
    );
}

// --snapshot without its address would produce evidence nobody can trace.
#[test]
#[allow(deprecated)]
fn a_snapshot_without_an_address_is_refused() {
    let root = tempdir().unwrap();
    let repository = root.path().join("kb");
    let provider = root.path().join("Personal Inbox");
    scaffold_personal_kb_v2(&repository).unwrap();
    fs::create_dir_all(&provider).unwrap();
    let text = root.path().join("page.txt");
    fs::write(&text, "The page said this.").unwrap();

    Command::cargo_bin("mko")
        .unwrap()
        .args(["add", "--snapshot"])
        .arg(&text)
        .args(["--title", "Example page", "--format", "json-v2"])
        .env("MKO_PERSONAL_PROVIDER_ROOT", &provider)
        .current_dir(&repository)
        .assert()
        .code(1)
        .stdout(predicates::str::contains("snapshot_arguments_incomplete"));
}

// Pasted text arrives in a file, not an argument — same file-not-argument
// discipline as `--snapshot` (§6, Phase 2).
#[test]
#[allow(deprecated)]
fn pasted_text_becomes_registered_evidence_with_no_locator() {
    let root = tempdir().unwrap();
    let repository = root.path().join("kb");
    let provider = root.path().join("Personal Inbox");
    scaffold_personal_kb_v2(&repository).unwrap();
    fs::create_dir_all(&provider).unwrap();
    let text = root.path().join("paste.txt");
    fs::write(&text, "the owner pasted this text").unwrap();

    let output = Command::cargo_bin("mko")
        .unwrap()
        .args(["add", "--paste"])
        .arg(&text)
        .args(["--title", "My paste", "--format", "json-v2"])
        .env("MKO_PERSONAL_PROVIDER_ROOT", &provider)
        .current_dir(&repository)
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let report: serde_json::Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(report["command"], "add");
    assert_eq!(report["data"]["outcome"], "created");
    // No locator exists for a paste (§6.1, decided): the empty-string
    // convention.
    assert_eq!(report["data"]["logical_locator"], "");

    let second = Command::cargo_bin("mko")
        .unwrap()
        .args(["add", "--paste"])
        .arg(&text)
        .args(["--title", "Same text again", "--format", "json-v2"])
        .env("MKO_PERSONAL_PROVIDER_ROOT", &provider)
        .current_dir(&repository)
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let second: serde_json::Value = serde_json::from_slice(&second).unwrap();
    assert_eq!(second["data"]["outcome"], "existing");
    assert_eq!(second["data"]["asset_id"], report["data"]["asset_id"]);
}

// A local Markdown/text file names the material itself: Core reads it
// directly and stores its original bytes content-addressed (§6.1). No
// separate runtime text file is written first, unlike `--paste`.
#[test]
#[allow(deprecated)]
fn a_local_markdown_file_is_registered_by_its_original_bytes() {
    let root = tempdir().unwrap();
    let repository = root.path().join("kb");
    let provider = root.path().join("Personal Inbox");
    scaffold_personal_kb_v2(&repository).unwrap();
    fs::create_dir_all(&provider).unwrap();
    let notes = root.path().join("owner-notes");
    fs::create_dir_all(&notes).unwrap();
    let note = notes.join("todo.md");
    fs::write(&note, "# TODO\n\n- write the phase 2 plan\n").unwrap();

    let output = Command::cargo_bin("mko")
        .unwrap()
        .args(["add", "--local-file"])
        .arg(&note)
        .args(["--format", "json-v2"])
        .env("MKO_PERSONAL_PROVIDER_ROOT", &provider)
        .current_dir(&repository)
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let report: serde_json::Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(report["command"], "add");
    assert_eq!(report["data"]["outcome"], "created");
    assert_eq!(report["data"]["logical_locator"], note.to_str().unwrap());
    let asset_id = report["data"]["asset_id"].as_str().unwrap();
    let hash = asset_id.strip_prefix("personal-asset-").unwrap();
    let stored = fs::read_to_string(
        repository
            .join("assets/originals")
            .join(format!("{hash}.md")),
    )
    .unwrap();
    assert_eq!(stored, "# TODO\n\n- write the phase 2 plan\n");
}

// Conversation content captured on a recall miss (§6.3, store-on-miss) is
// registered the same file-not-argument way as a paste or a snapshot.
#[test]
#[allow(deprecated)]
fn captured_conversation_content_becomes_registered_evidence() {
    let root = tempdir().unwrap();
    let repository = root.path().join("kb");
    let provider = root.path().join("Personal Inbox");
    scaffold_personal_kb_v2(&repository).unwrap();
    fs::create_dir_all(&provider).unwrap();
    let text = root.path().join("conversation.txt");
    fs::write(
        &text,
        "owner: what do we know about X?\nagent: nothing found; here is what I know...",
    )
    .unwrap();

    let output = Command::cargo_bin("mko")
        .unwrap()
        .args(["add", "--conversation"])
        .arg(&text)
        .args(["--format", "json-v2"])
        .env("MKO_PERSONAL_PROVIDER_ROOT", &provider)
        .current_dir(&repository)
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let report: serde_json::Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(report["command"], "add");
    assert_eq!(report["data"]["outcome"], "created");
    assert_eq!(report["data"]["logical_locator"], "");
}

/// A verified minimal valid 1x1 PNG (68 bytes) — real magic bytes, a real
/// zlib-compressed `IDAT` chunk, a real `IEND`. Small enough to embed as a
/// literal so image tests need no external fixture file.
fn tiny_png_bytes() -> Vec<u8> {
    vec![
        0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44,
        0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x04, 0x00, 0x00, 0x00, 0xb5,
        0x1c, 0x0c, 0x02, 0x00, 0x00, 0x00, 0x0b, 0x49, 0x44, 0x41, 0x54, 0x78, 0xda, 0x63, 0x64,
        0xf8, 0x0f, 0x00, 0x01, 0x05, 0x01, 0x01, 0x27, 0x18, 0xe3, 0x66, 0x00, 0x00, 0x00, 0x00,
        0x49, 0x45, 0x4e, 0x44, 0xae, 0x42, 0x60, 0x82,
    ]
}

// Phase 3, §6.1/§6.2: an image local file's original bytes are preserved
// signature-validated, distinct from a text local file's convention where the
// original bytes *are* the evidence.
#[test]
#[allow(deprecated)]
fn an_image_local_file_is_registered_with_its_signature_validated_original() {
    let root = tempdir().unwrap();
    let repository = root.path().join("kb");
    let provider = root.path().join("Personal Inbox");
    scaffold_personal_kb_v2(&repository).unwrap();
    fs::create_dir_all(&provider).unwrap();
    let screenshot = root.path().join("screenshot.png");
    let png = tiny_png_bytes();
    fs::write(&screenshot, &png).unwrap();

    let output = Command::cargo_bin("mko")
        .unwrap()
        .args(["add", "--local-file"])
        .arg(&screenshot)
        .args(["--title", "A screenshot", "--format", "json-v2"])
        .env("MKO_PERSONAL_PROVIDER_ROOT", &provider)
        .current_dir(&repository)
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let report: serde_json::Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(report["data"]["outcome"], "created");
    let asset_id = report["data"]["asset_id"].as_str().unwrap();
    let hash = asset_id.strip_prefix("personal-asset-").unwrap();
    let registry: serde_json::Value = serde_json::from_slice(
        &fs::read(
            repository
                .join("assets/registry")
                .join(format!("{asset_id}.json")),
        )
        .unwrap(),
    )
    .unwrap();
    assert_eq!(registry["media_type"], "image/png");
    assert_eq!(registry["origin"], "local_file");
    let stored = fs::read(
        repository
            .join("assets/originals")
            .join(format!("{hash}.png")),
    )
    .unwrap();
    assert_eq!(stored, png);
}

#[test]
#[allow(deprecated)]
fn a_local_file_with_a_mismatched_signature_is_rejected() {
    let root = tempdir().unwrap();
    let repository = root.path().join("kb");
    let provider = root.path().join("Personal Inbox");
    scaffold_personal_kb_v2(&repository).unwrap();
    fs::create_dir_all(&provider).unwrap();
    let fake = root.path().join("not-actually-a.png");
    fs::write(&fake, b"this is not PNG content at all").unwrap();

    let output = Command::cargo_bin("mko")
        .unwrap()
        .args(["add", "--local-file"])
        .arg(&fake)
        .args(["--format", "json-v2"])
        .env("MKO_PERSONAL_PROVIDER_ROOT", &provider)
        .current_dir(&repository)
        .assert()
        .code(1)
        .get_output()
        .stdout
        .clone();

    let output: serde_json::Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(output["error"]["code"], "local_file_signature_invalid");
    assert!(
        repository
            .join("assets/registry")
            .read_dir()
            .unwrap()
            .next()
            .is_none()
    );
}

// Phase 3, §6.2: an image's original carries no text of its own — `mko
// source prepare` requires the agent-read text via `--extracted-text`
// (a file, not an argument, matching every other evidence flag).
#[test]
#[allow(deprecated)]
fn source_prepare_requires_extracted_text_for_an_image_and_builds_a_bundle_from_it() {
    let root = tempdir().unwrap();
    let repository = root.path().join("kb");
    let provider = root.path().join("Personal Inbox");
    scaffold_personal_kb_v2(&repository).unwrap();
    fs::create_dir_all(&provider).unwrap();
    let screenshot = root.path().join("screenshot.png");
    fs::write(&screenshot, tiny_png_bytes()).unwrap();
    let add_output = Command::cargo_bin("mko")
        .unwrap()
        .args(["add", "--local-file"])
        .arg(&screenshot)
        .args(["--format", "json-v2"])
        .env("MKO_PERSONAL_PROVIDER_ROOT", &provider)
        .current_dir(&repository)
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let add_output: serde_json::Value = serde_json::from_slice(&add_output).unwrap();
    let asset_id = add_output["data"]["asset_id"].as_str().unwrap();

    // Without --extracted-text, Core refuses rather than guessing at text
    // this image's original does not carry.
    let missing_text = Command::cargo_bin("mko")
        .unwrap()
        .args(["source", "prepare", "--asset-id"])
        .arg(asset_id)
        .args(["--format", "json-v2"])
        .env("MKO_PERSONAL_PROVIDER_ROOT", &provider)
        .current_dir(&repository)
        .assert()
        .code(1)
        .get_output()
        .stdout
        .clone();
    let missing_text: serde_json::Value = serde_json::from_slice(&missing_text).unwrap();
    assert_eq!(
        missing_text["error"]["code"],
        "local_file_extracted_text_required"
    );

    let ocr = root.path().join("ocr.txt");
    fs::write(&ocr, "A screen reading: quarterly revenue up 12%.").unwrap();
    let prepared = Command::cargo_bin("mko")
        .unwrap()
        .args(["source", "prepare", "--asset-id"])
        .arg(asset_id)
        .arg("--extracted-text")
        .arg(&ocr)
        .args(["--format", "json-v2"])
        .env("MKO_PERSONAL_PROVIDER_ROOT", &provider)
        .current_dir(&repository)
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let prepared: serde_json::Value = serde_json::from_slice(&prepared).unwrap();
    assert_eq!(prepared["command"], "source.prepare");
    let bundle_path = prepared["data"]["bundle_path"].as_str().unwrap();
    let bundle: serde_json::Value =
        serde_json::from_slice(&fs::read(bundle_path).unwrap()).unwrap();
    assert_eq!(bundle["bundle"]["media_type"], "image/png");
    assert_eq!(
        bundle["bundle"]["content_blocks"][0]["text"],
        "A screen reading: quarterly revenue up 12%."
    );
}

// End-to-end: register an image, prepare it with agent-read (OCR) text,
// write the Source, then find it back with `--origin image` (§6, Phase 3).
#[test]
#[allow(deprecated)]
fn image_add_prepare_write_and_find_by_origin_flow() {
    let root = tempdir().unwrap();
    let repository = root.path().join("kb");
    let provider = root.path().join("Personal Inbox");
    scaffold_personal_kb_v2(&repository).unwrap();
    fs::create_dir_all(&provider).unwrap();
    let screenshot = root.path().join("screenshot.png");
    fs::write(&screenshot, tiny_png_bytes()).unwrap();
    let add_output = Command::cargo_bin("mko")
        .unwrap()
        .args(["add", "--local-file"])
        .arg(&screenshot)
        .args(["--title", "Revenue screenshot", "--format", "json-v2"])
        .env("MKO_PERSONAL_PROVIDER_ROOT", &provider)
        .current_dir(&repository)
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let add_output: serde_json::Value = serde_json::from_slice(&add_output).unwrap();
    let asset_id = add_output["data"]["asset_id"].as_str().unwrap();

    let ocr = root.path().join("ocr.txt");
    fs::write(&ocr, "Quarterly revenue increased by twelve percent.").unwrap();
    let prepared = Command::cargo_bin("mko")
        .unwrap()
        .args(["source", "prepare", "--asset-id"])
        .arg(asset_id)
        .arg("--extracted-text")
        .arg(&ocr)
        .args(["--format", "json-v2"])
        .env("MKO_PERSONAL_PROVIDER_ROOT", &provider)
        .current_dir(&repository)
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let prepared: serde_json::Value = serde_json::from_slice(&prepared).unwrap();
    let bundle_path = prepared["data"]["bundle_path"].as_str().unwrap();
    let bundle: serde_json::Value =
        serde_json::from_slice(&fs::read(bundle_path).unwrap()).unwrap();
    let block_id = bundle["bundle"]["content_blocks"][0]["id"]
        .as_str()
        .unwrap();
    let locator = bundle["bundle"]["content_blocks"][0]["locator"]
        .as_str()
        .unwrap();

    let response = serde_json::json!({
        "schema_version": 2,
        "title": "Revenue screenshot",
        "authors": [],
        "publication_date": null,
        "one_sentence_summary": "A screenshot showing a revenue increase.",
        "general_summary": "The screenshot reports quarterly revenue increased by twelve percent.",
        "key_claims": [{
            "text": "Quarterly revenue increased by twelve percent.",
            "evidence_refs": [{
                "block_id": block_id,
                "locator": locator,
                "text_span_utf8": null,
                "table_range": null,
            }],
        }],
        "limitations": [],
        "tags": ["revenue"],
        "knowledge_recommendation": {"outcome": "reference_only", "reasons": ["Single data point."]},
        "topics": [],
    });
    let response_path = root.path().join("source-response.json");
    fs::write(&response_path, serde_json::to_vec(&response).unwrap()).unwrap();
    Command::cargo_bin("mko")
        .unwrap()
        .args(["source", "write-draft", "--bundle"])
        .arg(bundle_path)
        .arg("--response")
        .arg(&response_path)
        .args(["--format", "json-v2"])
        .env("MKO_PERSONAL_PROVIDER_ROOT", &provider)
        .current_dir(&repository)
        .assert()
        .success();

    let matching = Command::cargo_bin("mko")
        .unwrap()
        .args([
            "find", "revenue", "--origin", "image", "--format", "json-v2",
        ])
        .env("MKO_PERSONAL_PROVIDER_ROOT", &provider)
        .current_dir(&repository)
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let matching: serde_json::Value = serde_json::from_slice(&matching).unwrap();
    assert_eq!(matching["data"]["items"].as_array().unwrap().len(), 1);
    assert_eq!(matching["data"]["items"][0]["asset_id"], asset_id);

    // The same query with an origin filter that cannot match an image
    // (`document`) returns nothing, proving the filter actually narrows.
    let non_matching = Command::cargo_bin("mko")
        .unwrap()
        .args([
            "find", "revenue", "--origin", "document", "--format", "json-v2",
        ])
        .env("MKO_PERSONAL_PROVIDER_ROOT", &provider)
        .current_dir(&repository)
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let non_matching: serde_json::Value = serde_json::from_slice(&non_matching).unwrap();
    assert!(non_matching["data"]["items"].as_array().unwrap().is_empty());
}

// A video's transcript arrives in a file, not an argument — the same
// discipline as `--snapshot` (Phase 4, §6). Same model as a web page: no
// original video bytes are ever fetched or stored.
#[test]
#[allow(deprecated)]
fn a_video_transcript_the_agent_read_becomes_registered_evidence() {
    let root = tempdir().unwrap();
    let repository = root.path().join("kb");
    let provider = root.path().join("Personal Inbox");
    scaffold_personal_kb_v2(&repository).unwrap();
    fs::create_dir_all(&provider).unwrap();
    let transcript = root.path().join("transcript.txt");
    fs::write(&transcript, "The speaker said this.").unwrap();

    let output = Command::cargo_bin("mko")
        .unwrap()
        .args(["add", "--video-transcript"])
        .arg(&transcript)
        .args([
            "--url",
            "https://www.youtube.com/watch?v=dQw4w9WgXcQ",
            "--title",
            "Example video",
            "--format",
            "json-v2",
        ])
        .env("MKO_PERSONAL_PROVIDER_ROOT", &provider)
        .current_dir(&repository)
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let report: serde_json::Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(report["command"], "add");
    assert_eq!(report["data"]["outcome"], "created");
    assert_eq!(
        report["data"]["logical_locator"],
        "https://www.youtube.com/watch?v=dQw4w9WgXcQ"
    );
    assert!(
        report["data"]["asset_id"]
            .as_str()
            .unwrap()
            .starts_with("personal-asset-")
    );
}

// `--video-transcript` without its address would produce evidence nobody can
// trace, exactly as `--snapshot` without `--url` would.
#[test]
#[allow(deprecated)]
fn a_video_transcript_without_an_address_is_refused() {
    let root = tempdir().unwrap();
    let repository = root.path().join("kb");
    let provider = root.path().join("Personal Inbox");
    scaffold_personal_kb_v2(&repository).unwrap();
    fs::create_dir_all(&provider).unwrap();
    let transcript = root.path().join("transcript.txt");
    fs::write(&transcript, "The speaker said this.").unwrap();

    Command::cargo_bin("mko")
        .unwrap()
        .args(["add", "--video-transcript"])
        .arg(&transcript)
        .args(["--title", "Example video", "--format", "json-v2"])
        .env("MKO_PERSONAL_PROVIDER_ROOT", &provider)
        .current_dir(&repository)
        .assert()
        .code(1)
        .stdout(predicates::str::contains(
            "video_transcript_arguments_incomplete",
        ));
}

// Registration through prepare and write-draft, then found by `--origin
// video` — the same round trip Phase 3 proved for `image`/`document`,
// completing the origin-form vocabulary (§6). A video transcript needs no
// `--extracted-text` at prepare: unlike an image or document, the transcript
// text supplied at registration already *is* the evidence.
#[test]
#[allow(deprecated)]
fn video_add_prepare_write_and_find_by_origin_flow() {
    let root = tempdir().unwrap();
    let repository = root.path().join("kb");
    let provider = root.path().join("Personal Inbox");
    scaffold_personal_kb_v2(&repository).unwrap();
    fs::create_dir_all(&provider).unwrap();
    let transcript = root.path().join("transcript.txt");
    fs::write(
        &transcript,
        "The speaker said quarterly revenue increased by twelve percent.",
    )
    .unwrap();

    let add_output = Command::cargo_bin("mko")
        .unwrap()
        .args(["add", "--video-transcript"])
        .arg(&transcript)
        .args([
            "--url",
            "https://www.youtube.com/watch?v=dQw4w9WgXcQ",
            "--title",
            "Earnings call video",
            "--format",
            "json-v2",
        ])
        .env("MKO_PERSONAL_PROVIDER_ROOT", &provider)
        .current_dir(&repository)
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let add_output: serde_json::Value = serde_json::from_slice(&add_output).unwrap();
    let asset_id = add_output["data"]["asset_id"].as_str().unwrap();

    let prepared = Command::cargo_bin("mko")
        .unwrap()
        .args(["source", "prepare", "--asset-id"])
        .arg(asset_id)
        .args(["--format", "json-v2"])
        .env("MKO_PERSONAL_PROVIDER_ROOT", &provider)
        .current_dir(&repository)
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let prepared: serde_json::Value = serde_json::from_slice(&prepared).unwrap();
    let bundle_path = prepared["data"]["bundle_path"].as_str().unwrap();
    let bundle: serde_json::Value =
        serde_json::from_slice(&fs::read(bundle_path).unwrap()).unwrap();
    let block_id = bundle["bundle"]["content_blocks"][0]["id"]
        .as_str()
        .unwrap();
    let locator = bundle["bundle"]["content_blocks"][0]["locator"]
        .as_str()
        .unwrap();

    let response = serde_json::json!({
        "schema_version": 2,
        "title": "Earnings call video",
        "authors": [],
        "publication_date": null,
        "one_sentence_summary": "A video reporting a revenue increase.",
        "general_summary": "The video reports quarterly revenue increased by twelve percent.",
        "key_claims": [{
            "text": "Quarterly revenue increased by twelve percent.",
            "evidence_refs": [{
                "block_id": block_id,
                "locator": locator,
                "text_span_utf8": null,
                "table_range": null,
            }],
        }],
        "limitations": [],
        "tags": ["revenue"],
        "knowledge_recommendation": {"outcome": "reference_only", "reasons": ["Single data point."]},
        "topics": [],
    });
    let response_path = root.path().join("source-response.json");
    fs::write(&response_path, serde_json::to_vec(&response).unwrap()).unwrap();
    Command::cargo_bin("mko")
        .unwrap()
        .args(["source", "write-draft", "--bundle"])
        .arg(bundle_path)
        .arg("--response")
        .arg(&response_path)
        .args(["--format", "json-v2"])
        .env("MKO_PERSONAL_PROVIDER_ROOT", &provider)
        .current_dir(&repository)
        .assert()
        .success();

    let matching = Command::cargo_bin("mko")
        .unwrap()
        .args([
            "find", "revenue", "--origin", "video", "--format", "json-v2",
        ])
        .env("MKO_PERSONAL_PROVIDER_ROOT", &provider)
        .current_dir(&repository)
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let matching: serde_json::Value = serde_json::from_slice(&matching).unwrap();
    assert_eq!(matching["data"]["items"].as_array().unwrap().len(), 1);
    assert_eq!(matching["data"]["items"][0]["asset_id"], asset_id);

    // The same query with an origin filter that cannot match a video (`web`)
    // returns nothing, proving the filter actually narrows.
    let non_matching = Command::cargo_bin("mko")
        .unwrap()
        .args(["find", "revenue", "--origin", "web", "--format", "json-v2"])
        .env("MKO_PERSONAL_PROVIDER_ROOT", &provider)
        .current_dir(&repository)
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let non_matching: serde_json::Value = serde_json::from_slice(&non_matching).unwrap();
    assert!(non_matching["data"]["items"].as_array().unwrap().is_empty());
}
