use std::path::PathBuf;

fn capture_skill_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../skills/codex/capture-asset/SKILL.md")
}

fn process_skill_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../skills/codex/process-asset/SKILL.md")
}

fn executable_shell_blocks(markdown: &str) -> String {
    let mut output = String::new();
    let mut in_shell_block = false;

    for line in markdown.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("```") {
            if in_shell_block {
                in_shell_block = false;
            } else {
                let language = trimmed.trim_start_matches('`').trim();
                in_shell_block = matches!(language, "bash" | "sh" | "shell" | "zsh" | "powershell");
            }
            continue;
        }
        if in_shell_block {
            output.push_str(line);
            output.push('\n');
        }
    }

    output
}

#[test]
fn capture_skill_exposes_only_the_stable_capture_command() {
    let text = std::fs::read_to_string(capture_skill_path())
        .expect("capture-asset Skill must exist and be readable");
    let commands = executable_shell_blocks(&text);

    assert!(commands.contains("mko asset capture"));
    for forbidden in [
        "source prepare",
        "source write-draft",
        "human approve-source",
        "asset accept-change",
        "git commit",
        "git push",
        "curl ",
        "wget ",
        "cp ",
        "copy ",
    ] {
        assert!(
            !commands.contains(forbidden),
            "capture Skill exposes forbidden shell operation: {forbidden}"
        );
    }
}

#[test]
fn codex_skills_never_expose_human_approval_or_publication() {
    for path in [capture_skill_path(), process_skill_path()] {
        let text = std::fs::read_to_string(&path).unwrap_or_else(|error| {
            panic!("{} must exist and be readable: {error}", path.display())
        });
        let commands = executable_shell_blocks(&text);

        for forbidden in [
            "approve-source",
            "accept-change",
            "git commit",
            "git push",
            "curl ",
            "wget ",
        ] {
            assert!(
                !commands.contains(forbidden),
                "{} exposes forbidden shell operation: {forbidden}",
                path.display()
            );
        }
    }
}

#[test]
fn process_skill_defines_the_pending_source_workflow() {
    let path = process_skill_path();
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("{} must exist and be readable: {error}", path.display()));
    let commands = executable_shell_blocks(&text);

    for required in ["mko source prepare", "mko source write-draft", "mko check"] {
        assert!(
            commands.contains(required),
            "process Skill must expose stable command: {required}"
        );
    }
    for required_guidance in [
        "Personal Scope",
        "untrusted_document_text",
        "semantic-response-v1",
        "General Summary",
        "Domain Perspective",
        "evidence",
        "Uncertainties",
        "Promotion candidates",
        "pending Source",
    ] {
        assert!(
            text.contains(required_guidance),
            "process Skill is missing required workflow guidance: {required_guidance}"
        );
    }
}
