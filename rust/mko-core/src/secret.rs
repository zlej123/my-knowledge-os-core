use std::{path::Path, sync::OnceLock};

use regex::RegexSet;
use serde::Serialize;

const RULES: [(&str, &str); 4] = [
    ("openai_api_key", r"\bsk-[A-Za-z0-9_-]{20,}\b"),
    (
        "github_token",
        r"\b(?:gh[pousr]_[A-Za-z0-9]{20,}|github_pat_[A-Za-z0-9_]{20,})\b",
    ),
    ("bearer_token", r"(?i)\bbearer[ \t]+[A-Za-z0-9._~+/=-]{12,}"),
    (
        "pem_private_key",
        r"-----BEGIN (?:RSA |EC |OPENSSH )?PRIVATE KEY-----",
    ),
];

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct SecretFinding {
    pub rule: String,
}

pub fn scan(path: &Path, bytes: &[u8]) -> Vec<SecretFinding> {
    let mut findings = Vec::new();
    if let Some(rule) = credential_filename_rule(path) {
        findings.push(SecretFinding { rule: rule.into() });
    }
    let text = String::from_utf8_lossy(bytes);
    for index in patterns().matches(&text).iter() {
        findings.push(SecretFinding {
            rule: RULES[index].0.into(),
        });
    }
    findings.sort_by(|left, right| left.rule.cmp(&right.rule));
    findings.dedup();
    findings
}

fn patterns() -> &'static RegexSet {
    static PATTERNS: OnceLock<RegexSet> = OnceLock::new();
    PATTERNS.get_or_init(|| {
        RegexSet::new(RULES.map(|(_, pattern)| pattern))
            .expect("static secret patterns must compile")
    })
}

fn credential_filename_rule(path: &Path) -> Option<&'static str> {
    let name = path.file_name()?.to_str()?.to_ascii_lowercase();
    if name == ".env" || name.starts_with(".env.") {
        return Some("environment_file");
    }
    if name.contains("credential")
        || name.contains("password")
        || name == "id_rsa"
        || name == "id_ed25519"
    {
        return Some("credential_filename");
    }
    None
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::scan;

    #[test]
    fn reports_rule_names_without_retaining_values() {
        let findings = scan(
            Path::new("notes/example.md"),
            b"Bearer abcdefghijklmnopqrstuv",
        );
        assert_eq!(findings[0].rule, "bearer_token");
        assert!(!format!("{findings:?}").contains("abcdefghijkl"));
    }
}
