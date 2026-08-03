use std::{
    collections::BTreeMap,
    fs::{self, OpenOptions},
    io::{BufRead, BufReader, IsTerminal, Read, Write},
    path::{Path, PathBuf},
    time::{Duration as StdDuration, Instant},
};

#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;
#[cfg(windows)]
use std::os::windows::fs::OpenOptionsExt;

use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};

use crate::{
    clock::Clock,
    config_v2::KnowledgeConfigV2,
    context::{PlatformEnvironment, Scope},
    error::MkoError,
    profile::{PersonalProfile, ProfileStore},
    revision_v2::{canonical_json_bytes, canonical_json_sha256, sha256_digest},
    setup_v2::{SetupPersonalV2Request, SetupPersonalV2Result, setup_personal_v2_locked},
};

const PLAN_TTL: Duration = Duration::minutes(15);
const MAX_PLAN_BYTES: u64 = 256 * 1024;
const MAX_OPEN_PLANS: usize = 64;
const PLAN_SCAN_DEADLINE: StdDuration = StdDuration::from_millis(100);
const PLAN_PREFIX: &str = "mko-setup-plan-";
const PROFILE_NAME: &str = "personal";
const INBOX_COMPONENTS: [&str; 3] = ["My-Knowledge-OS-Assets", "personal", "inbox"];
const MANAGED_DIRECTORIES: &[&str] = &[
    "assets",
    "assets/registry",
    "assets/attempts",
    "sources",
    "knowledge",
    "reviews",
    "views",
    "views/records",
    ".mko",
    "recovery",
    "recovery/manual-edits",
];
const GENERATED_FILES: &[(&str, &str)] = &[
    (".mko/.gitignore", "runtime/\n"),
    (
        "HOME.md",
        r#"---
type: dashboard
generated_by: my-knowledge-os
---

# My Knowledge OS

## 검토 대기

![[views/review-queue.base]]

## 승인된 지식

![[views/knowledge-library.base]]

터미널에서는 `mko queue`로 같은 검토 대기열을 확인할 수 있습니다.
"#,
    ),
    (
        "views/review-queue.base",
        r#"filters:
  and:
    - file.inFolder("views/records")
    - 'derived_state != "approved"'
views:
  - type: table
    name: Review Queue
    order:
      - title
      - record_type
      - derived_state
      - domain
      - perspectives
      - current_revision
"#,
    ),
    (
        "views/knowledge-library.base",
        r#"filters:
  and:
    - file.inFolder("views/records")
    - 'record_type == "knowledge"'
    - 'derived_state == "approved"'
properties:
  perspectives:
    displayName: 관점
views:
  - type: table
    name: 전체 지식
    order:
      - title
      - perspectives
      - tags
      - current_revision
  - type: table
    name: 생활
    filters:
      and:
        - 'list(perspectives).contains("life")'
    order:
      - title
      - perspectives
      - tags
      - current_revision
  - type: table
    name: 학습
    filters:
      and:
        - 'list(perspectives).contains("learning")'
    order:
      - title
      - perspectives
      - tags
      - current_revision
  - type: table
    name: 기술
    filters:
      and:
        - 'list(perspectives).contains("technical")'
    order:
      - title
      - perspectives
      - tags
      - current_revision
  - type: table
    name: 프로젝트
    filters:
      and:
        - 'list(perspectives).contains("project")'
    order:
      - title
      - perspectives
      - tags
      - current_revision
  - type: table
    name: 투자
    filters:
      and:
        - 'list(perspectives).contains("investment")'
    order:
      - title
      - perspectives
      - tags
      - current_revision
"#,
    ),
];

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SetupPlanStepIdV2 {
    ScaffoldRepository,
    EnsureProviderInbox,
    EnsureDashboard,
    ConfigureProfile,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SetupPlanEffectV2 {
    Read,
    Create,
    Modify,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SetupPlanApprovalModeV2 {
    Tty,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SetupPlanNextActionV2 {
    ApprovePlan,
    None,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SetupPlanStepV2 {
    pub step_id: SetupPlanStepIdV2,
    pub effect: SetupPlanEffectV2,
    pub destination: String,
    pub requires_human_approval: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SetupPlanDataV2 {
    pub plan_id: String,
    pub expires_at: DateTime<Utc>,
    pub single_use: bool,
    pub precondition_digest: String,
    pub effect_digest: String,
    pub steps: Vec<SetupPlanStepV2>,
    pub approval_mode: SetupPlanApprovalModeV2,
    pub next_action: SetupPlanNextActionV2,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SetupPlanApplyResultV2 {
    pub plan_id: String,
    pub setup: SetupPersonalV2Result,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct StoredSetupPlanV2 {
    schema_version: u32,
    plan_id: String,
    created_at: DateTime<Utc>,
    expires_at: DateTime<Utc>,
    repository_root: PathBuf,
    drive_account_root: PathBuf,
    replace_profile: bool,
    precondition_digest: String,
    effect_digest: String,
    steps: Vec<SetupPlanStepV2>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct SetupPreconditionV2 {
    product_version: String,
    repository_root: String,
    drive_account_root: String,
    provider_root: String,
    profile_path: String,
    observations: BTreeMap<String, PathObservationV2>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
enum PathObservationV2 {
    Missing,
    Directory,
    File { size: u64, digest: String },
}

struct InspectedSetupV2 {
    repository_root: PathBuf,
    drive_account_root: PathBuf,
    precondition_digest: String,
    steps: Vec<SetupPlanStepV2>,
}

struct PlanDirectories {
    open: PathBuf,
    consumed: PathBuf,
}

/// Creates a machine-local, owner-only, expiring setup plan without mutating the KB, provider,
/// dashboard, or profile targets named by that plan.
pub fn create_setup_plan_v2(
    request: SetupPersonalV2Request<'_>,
    platform: &dyn PlatformEnvironment,
    clock: &dyn Clock,
) -> Result<SetupPlanDataV2, MkoError> {
    let store = ProfileStore::from_platform(platform)?;
    let _mutation_lock = store.acquire_mutation_lock()?;
    let directories = ensure_plan_directories(platform)?;
    let created_at = clock.now_utc();
    cleanup_expired_open_plans(&directories, created_at)?;
    enforce_open_plan_bound(&directories.open)?;
    let inspected = inspect_setup(&request, platform)?;
    let plan_id = new_plan_id()?;
    let expires_at = created_at + PLAN_TTL;
    let effect_digest = canonical_json_sha256(&inspected.steps)?;
    let stored = StoredSetupPlanV2 {
        schema_version: 2,
        plan_id: plan_id.clone(),
        created_at,
        expires_at,
        repository_root: inspected.repository_root,
        drive_account_root: inspected.drive_account_root,
        replace_profile: request.replace_profile,
        precondition_digest: inspected.precondition_digest.clone(),
        effect_digest: effect_digest.clone(),
        steps: inspected.steps.clone(),
    };
    let bytes = canonical_json_bytes(&stored)?;
    if bytes.len() as u64 > MAX_PLAN_BYTES {
        return Err(MkoError::new(
            "setup_plan_too_large",
            "the setup plan exceeds its bounded local representation",
        ));
    }
    write_private_new(&directories.open.join(format!("{plan_id}.json")), &bytes)?;
    let next_action = if inspected
        .steps
        .iter()
        .any(|step| step.requires_human_approval)
    {
        SetupPlanNextActionV2::ApprovePlan
    } else {
        SetupPlanNextActionV2::None
    };
    Ok(SetupPlanDataV2 {
        plan_id,
        expires_at,
        single_use: true,
        precondition_digest: inspected.precondition_digest,
        effect_digest,
        steps: inspected.steps,
        approval_mode: SetupPlanApprovalModeV2::Tty,
        next_action,
    })
}

/// Applies an exact still-current Core-issued setup plan only after Core displays and confirms it
/// on the process's real terminal. There is intentionally no non-TTY mutation entry point.
pub fn apply_setup_plan_v2_tty(
    plan_id: &str,
    platform: &dyn PlatformEnvironment,
    clock: &dyn Clock,
) -> Result<SetupPlanApplyResultV2, MkoError> {
    let mut terminal = ProcessTty;
    apply_setup_plan_with_terminal(plan_id, platform, clock, &mut terminal)
}

fn apply_setup_plan_with_terminal(
    plan_id: &str,
    platform: &dyn PlatformEnvironment,
    clock: &dyn Clock,
    terminal: &mut dyn SetupTtyInteraction,
) -> Result<SetupPlanApplyResultV2, MkoError> {
    validate_plan_id(plan_id)?;
    let store = ProfileStore::from_platform(platform)?;
    let mutation_lock = store.acquire_mutation_lock()?;
    let directories = ensure_plan_directories(platform)?;
    let filename = format!("{plan_id}.json");
    let consumed_path = directories.consumed.join(&filename);
    if path_exists_nofollow(&consumed_path)? {
        require_private_regular_file(&consumed_path)?;
        return Err(consumed_error());
    }
    let open_path = directories.open.join(&filename);
    let stored = read_plan(&open_path)?;
    if stored.schema_version != 2 || stored.plan_id != plan_id {
        return Err(MkoError::new(
            "setup_plan_invalid",
            "the machine-local setup plan identity is invalid",
        ));
    }
    if clock.now_utc() >= stored.expires_at {
        return Err(MkoError::new(
            "setup_plan_expired",
            "the setup plan expired; create and approve a new plan",
        ));
    }

    validate_stored_plan_current(&stored, platform)?;
    let approval = prepare_setup_approval(&stored, &store)?;
    confirm_setup_approval(&approval, terminal)?;

    let current_stored = read_plan(&open_path).map_err(|_| stale_error())?;
    if current_stored != stored || clock.now_utc() >= stored.expires_at {
        return Err(stale_error());
    }
    let expected_profile = store.read_snapshot()?;
    validate_stored_plan_current(&stored, platform)?;

    consume_plan(&open_path, &consumed_path)?;
    let request = SetupPersonalV2Request {
        repository_root: &stored.repository_root,
        drive_account_root: &stored.drive_account_root,
        replace_profile: stored.replace_profile,
    };
    let setup =
        setup_personal_v2_locked(request, platform, &store, &mutation_lock, expected_profile)?;
    Ok(SetupPlanApplyResultV2 {
        plan_id: plan_id.to_owned(),
        setup,
    })
}

fn validate_stored_plan_current(
    stored: &StoredSetupPlanV2,
    platform: &dyn PlatformEnvironment,
) -> Result<(), MkoError> {
    let request = SetupPersonalV2Request {
        repository_root: &stored.repository_root,
        drive_account_root: &stored.drive_account_root,
        replace_profile: stored.replace_profile,
    };
    let inspected = inspect_setup(&request, platform).map_err(|_| stale_error())?;
    let current_effect_digest = canonical_json_sha256(&inspected.steps)?;
    if inspected.precondition_digest != stored.precondition_digest
        || current_effect_digest != stored.effect_digest
        || inspected.steps != stored.steps
    {
        return Err(stale_error());
    }
    Ok(())
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct SetupApprovalPathsV2 {
    repository_root: String,
    drive_account_root: String,
    provider_inbox: String,
    profile_path: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct SetupApprovalEffectV2 {
    schema_version: u32,
    operation: &'static str,
    plan_id: String,
    expires_at: DateTime<Utc>,
    precondition_digest: String,
    effect_digest: String,
    card_digest: String,
    paths: SetupApprovalPathsV2,
    steps: Vec<SetupPlanStepV2>,
}

struct PreparedSetupApprovalV2 {
    card_bytes: Vec<u8>,
    card_digest: String,
    approval_effect_digest: String,
}

fn prepare_setup_approval(
    stored: &StoredSetupPlanV2,
    store: &ProfileStore,
) -> Result<PreparedSetupApprovalV2, MkoError> {
    let provider_inbox = INBOX_COMPONENTS
        .iter()
        .fold(stored.drive_account_root.clone(), |path, component| {
            path.join(component)
        });
    let profile_parent = store.path().parent().ok_or_else(|| {
        MkoError::new(
            "profile_path_invalid",
            "machine profile path has no parent directory",
        )
    })?;
    let profile_parent = fs::canonicalize(profile_parent)
        .map_err(|error| MkoError::new("profile_path_invalid", error.to_string()))?;
    let profile_name = store.path().file_name().ok_or_else(|| {
        MkoError::new(
            "profile_path_invalid",
            "machine profile path has no file name",
        )
    })?;
    let profile_path = profile_parent.join(profile_name);
    let paths = SetupApprovalPathsV2 {
        repository_root: path_text(&stored.repository_root)?,
        drive_account_root: path_text(&stored.drive_account_root)?,
        provider_inbox: path_text(&provider_inbox)?,
        profile_path: path_text(&profile_path)?,
    };
    let mut card = String::new();
    use std::fmt::Write as _;
    writeln!(card, "# My Knowledge OS setup approval\n")
        .map_err(|error| MkoError::new("setup_tty_failed", error.to_string()))?;
    writeln!(card, "- Plan ID: `{}`", stored.plan_id)
        .and_then(|()| writeln!(card, "- Expires at: `{}`", stored.expires_at.to_rfc3339()))
        .and_then(|()| {
            writeln!(
                card,
                "- Precondition digest: `{}`",
                stored.precondition_digest
            )
        })
        .and_then(|()| writeln!(card, "- Effect digest: `{}`\n", stored.effect_digest))
        .map_err(|error| MkoError::new("setup_tty_failed", error.to_string()))?;
    card.push_str("## Exact canonical paths\n\n");
    for (label, path) in [
        ("Repository root", &paths.repository_root),
        ("Drive account root", &paths.drive_account_root),
        ("Provider Inbox", &paths.provider_inbox),
        ("Machine profile", &paths.profile_path),
    ] {
        writeln!(
            card,
            "- {label}: {}",
            serde_json::to_string(path)
                .map_err(|error| MkoError::new("setup_tty_failed", error.to_string()))?
        )
        .map_err(|error| MkoError::new("setup_tty_failed", error.to_string()))?;
    }
    card.push_str("\n## Exact effects\n\n");
    for step in &stored.steps {
        let destination = match step.step_id {
            SetupPlanStepIdV2::ScaffoldRepository | SetupPlanStepIdV2::EnsureDashboard => {
                &paths.repository_root
            }
            SetupPlanStepIdV2::EnsureProviderInbox => &paths.provider_inbox,
            SetupPlanStepIdV2::ConfigureProfile => &paths.profile_path,
        };
        writeln!(
            card,
            "- `{:?}`: `{:?}` at {}",
            step.step_id,
            step.effect,
            serde_json::to_string(destination)
                .map_err(|error| MkoError::new("setup_tty_failed", error.to_string()))?
        )
        .map_err(|error| MkoError::new("setup_tty_failed", error.to_string()))?;
    }
    let card_bytes = card.into_bytes();
    let card_digest = sha256_digest(&card_bytes);
    let effect = SetupApprovalEffectV2 {
        schema_version: 2,
        operation: "setup_apply",
        plan_id: stored.plan_id.clone(),
        expires_at: stored.expires_at,
        precondition_digest: stored.precondition_digest.clone(),
        effect_digest: stored.effect_digest.clone(),
        card_digest: card_digest.clone(),
        paths,
        steps: stored.steps.clone(),
    };
    Ok(PreparedSetupApprovalV2 {
        card_bytes,
        card_digest,
        approval_effect_digest: canonical_json_sha256(&effect)?,
    })
}

trait SetupTtyInteraction {
    fn is_real_tty(&self) -> bool;
    fn display(&mut self, bytes: &[u8]) -> std::io::Result<()>;
    fn read_confirmation(&mut self, byte_limit: u64) -> std::io::Result<String>;
}

struct ProcessTty;

impl SetupTtyInteraction for ProcessTty {
    fn is_real_tty(&self) -> bool {
        std::io::stdin().is_terminal() && std::io::stderr().is_terminal()
    }

    fn display(&mut self, bytes: &[u8]) -> std::io::Result<()> {
        let mut stderr = std::io::stderr().lock();
        stderr.write_all(bytes)?;
        stderr.flush()
    }

    fn read_confirmation(&mut self, byte_limit: u64) -> std::io::Result<String> {
        let stdin = std::io::stdin().lock();
        let mut bounded = BufReader::new(stdin.take(byte_limit));
        let mut input = String::new();
        bounded.read_line(&mut input)?;
        Ok(input)
    }
}

fn confirm_setup_approval(
    approval: &PreparedSetupApprovalV2,
    terminal: &mut dyn SetupTtyInteraction,
) -> Result<(), MkoError> {
    let phrase = format!(
        "approve-setup {} {}",
        approval.card_digest, approval.approval_effect_digest
    );
    let mut display = Vec::with_capacity(approval.card_bytes.len() + 512);
    display.extend_from_slice(b"\n");
    display.extend_from_slice(&approval.card_bytes);
    write!(
        display,
        "\nCard digest: {}\nApproval effect digest: {}\n\nType exactly:\n{}\n> ",
        approval.card_digest, approval.approval_effect_digest, phrase
    )
    .map_err(|error| MkoError::new("setup_tty_failed", error.to_string()))?;
    terminal
        .display(&display)
        .map_err(|error| MkoError::new("setup_tty_failed", error.to_string()))?;
    if !terminal.is_real_tty() {
        return Err(MkoError::new(
            "setup_tty_required",
            "setup apply requires Core-owned display and confirmation on a real TTY",
        ));
    }
    let input = terminal
        .read_confirmation(512)
        .map_err(|error| MkoError::new("setup_tty_failed", error.to_string()))?;
    let input = input
        .strip_suffix("\r\n")
        .or_else(|| input.strip_suffix('\n'))
        .or_else(|| input.strip_suffix('\r'))
        .unwrap_or(&input);
    if input != phrase {
        return Err(MkoError::new(
            "setup_confirmation_mismatch",
            "the exact Core-rendered setup approval phrase was not entered",
        ));
    }
    Ok(())
}

fn inspect_setup(
    request: &SetupPersonalV2Request<'_>,
    platform: &dyn PlatformEnvironment,
) -> Result<InspectedSetupV2, MkoError> {
    let drive_account_root = canonical_real_directory(request.drive_account_root)?;
    ensure_writable(&drive_account_root, "provider_root_invalid")?;
    let provider_root = INBOX_COMPONENTS
        .iter()
        .fold(drive_account_root.clone(), |path, component| {
            path.join(component)
        });
    validate_provider_path(&drive_account_root, &provider_root)?;

    let repository_root = destination_candidate(request.repository_root, platform)?;
    if repository_root.starts_with(&drive_account_root)
        || provider_root.starts_with(&repository_root)
    {
        return Err(MkoError::new(
            "storage_roots_overlap",
            "the Git KB must remain outside the selected Drive account",
        ));
    }
    validate_repository_destination(&repository_root)?;

    let store = ProfileStore::from_platform(platform)?;
    let profiles = store.read()?;
    let desired = PersonalProfile {
        repository_root: repository_root.clone(),
        provider_root: provider_root.clone(),
        scope: Scope::Personal,
    };
    let existing = profiles
        .as_ref()
        .and_then(|profile| profile.profiles.get(PROFILE_NAME));
    if existing.is_some_and(|profile| profile != &desired) && !request.replace_profile {
        return Err(MkoError::new(
            "profile_conflict",
            "a different Personal profile exists; rerun setup after explicitly choosing replacement",
        ));
    }

    let mut observations = BTreeMap::new();
    observations.insert(
        "repository:root".into(),
        observe_path(&repository_root, "repository_root_invalid")?,
    );
    for relative in MANAGED_DIRECTORIES {
        observations.insert(
            format!("repository:{relative}"),
            observe_path(&repository_root.join(relative), "kb_destination_invalid")?,
        );
    }
    observations.insert(
        "repository:knowledge-os.yaml".into(),
        observe_path(
            &repository_root.join("knowledge-os.yaml"),
            "kb_config_invalid",
        )?,
    );
    for (relative, _) in GENERATED_FILES {
        observations.insert(
            format!("repository:{relative}"),
            observe_path(&repository_root.join(relative), "dashboard_drift")?,
        );
    }
    observations.insert(
        "provider:account".into(),
        observe_path(&drive_account_root, "provider_root_invalid")?,
    );
    let mut provider_component = drive_account_root.clone();
    for component in INBOX_COMPONENTS {
        provider_component.push(component);
        observations.insert(
            format!("provider:{component}"),
            observe_path(&provider_component, "provider_root_invalid")?,
        );
    }
    observations.insert(
        "profile:file".into(),
        observe_path(store.path(), "profile_path_invalid")?,
    );

    let repository_effect = repository_effect(&repository_root)?;
    let provider_effect = if provider_root.exists() {
        SetupPlanEffectV2::Read
    } else {
        SetupPlanEffectV2::Create
    };
    let dashboard_effect = dashboard_effect(&repository_root)?;
    let profile_effect = match existing {
        None => SetupPlanEffectV2::Create,
        Some(profile) if profile == &desired => SetupPlanEffectV2::Read,
        Some(_) => SetupPlanEffectV2::Modify,
    };
    let steps = vec![
        setup_step(
            SetupPlanStepIdV2::ScaffoldRepository,
            repository_effect,
            "personal-kb",
        ),
        setup_step(
            SetupPlanStepIdV2::EnsureProviderInbox,
            provider_effect,
            "provider:personal/inbox",
        ),
        setup_step(
            SetupPlanStepIdV2::EnsureDashboard,
            dashboard_effect,
            "personal-kb:obsidian-dashboard",
        ),
        setup_step(
            SetupPlanStepIdV2::ConfigureProfile,
            profile_effect,
            "machine-profile:personal",
        ),
    ];
    let precondition = SetupPreconditionV2 {
        product_version: env!("CARGO_PKG_VERSION").into(),
        repository_root: path_text(&repository_root)?,
        drive_account_root: path_text(&drive_account_root)?,
        provider_root: path_text(&provider_root)?,
        profile_path: path_text(store.path())?,
        observations,
    };
    Ok(InspectedSetupV2 {
        repository_root,
        drive_account_root,
        precondition_digest: canonical_json_sha256(&precondition)?,
        steps,
    })
}

fn setup_step(
    step_id: SetupPlanStepIdV2,
    effect: SetupPlanEffectV2,
    destination: &str,
) -> SetupPlanStepV2 {
    SetupPlanStepV2 {
        step_id,
        effect,
        destination: destination.into(),
        requires_human_approval: effect != SetupPlanEffectV2::Read,
    }
}

fn repository_effect(repository_root: &Path) -> Result<SetupPlanEffectV2, MkoError> {
    if !repository_root.exists() {
        return Ok(SetupPlanEffectV2::Create);
    }
    let all_present = MANAGED_DIRECTORIES
        .iter()
        .all(|relative| repository_root.join(relative).is_dir())
        && repository_root.join("knowledge-os.yaml").is_file()
        && repository_root.join(".mko/.gitignore").is_file();
    Ok(if all_present {
        SetupPlanEffectV2::Read
    } else {
        SetupPlanEffectV2::Modify
    })
}

fn dashboard_effect(repository_root: &Path) -> Result<SetupPlanEffectV2, MkoError> {
    let mut missing = false;
    for (relative, expected) in &GENERATED_FILES[1..] {
        let path = repository_root.join(relative);
        match fs::symlink_metadata(&path) {
            Ok(metadata) if metadata.is_file() && !metadata.file_type().is_symlink() => {
                if fs::read(&path)
                    .map_err(|error| MkoError::new("dashboard_drift", error.to_string()))?
                    != expected.as_bytes()
                {
                    return Err(MkoError::new(
                        "dashboard_drift",
                        format!("generated dashboard {relative} was edited"),
                    ));
                }
            }
            Ok(_) => {
                return Err(MkoError::new(
                    "dashboard_drift",
                    format!("generated dashboard path {relative} is not a regular file"),
                ));
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => missing = true,
            Err(error) => return Err(MkoError::new("dashboard_drift", error.to_string())),
        }
    }
    Ok(if missing {
        SetupPlanEffectV2::Create
    } else {
        SetupPlanEffectV2::Read
    })
}

fn validate_repository_destination(path: &Path) -> Result<(), MkoError> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(MkoError::new("repository_root_invalid", error.to_string())),
    };
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err(MkoError::new(
            "kb_destination_invalid",
            "the KB destination must be a real directory",
        ));
    }
    let marker = path.join("knowledge-os.yaml");
    if marker.exists() {
        KnowledgeConfigV2::read(path)?;
    } else if fs::read_dir(path)
        .map_err(|error| MkoError::new("kb_destination_invalid", error.to_string()))?
        .next()
        .transpose()
        .map_err(|error| MkoError::new("kb_destination_invalid", error.to_string()))?
        .is_some()
    {
        return Err(MkoError::new(
            "kb_destination_not_empty",
            "choose an empty directory or an existing v0.3 Personal KB",
        ));
    }
    for relative in MANAGED_DIRECTORIES {
        reject_non_directory_if_present(&path.join(relative), "kb_path_invalid")?;
    }
    validate_expected_file(
        &path.join(".mko/.gitignore"),
        b"runtime/\n",
        "kb_runtime_policy_invalid",
    )?;
    Ok(())
}

fn validate_provider_path(account_root: &Path, inbox: &Path) -> Result<(), MkoError> {
    if !inbox.starts_with(account_root) {
        return Err(MkoError::new(
            "provider_root_invalid",
            "Personal Inbox must remain inside the selected Drive account",
        ));
    }
    let mut current = account_root.to_path_buf();
    let mut missing = false;
    for component in INBOX_COMPONENTS {
        current.push(component);
        if missing {
            continue;
        }
        match fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {}
            Ok(_) => {
                return Err(MkoError::new(
                    "provider_root_invalid",
                    "Personal Inbox components must be real directories",
                ));
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => missing = true,
            Err(error) => return Err(MkoError::new("provider_root_invalid", error.to_string())),
        }
    }
    Ok(())
}

fn destination_candidate(
    path: &Path,
    platform: &dyn PlatformEnvironment,
) -> Result<PathBuf, MkoError> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        platform.current_dir()?.join(path)
    };
    if absolute
        .components()
        .any(|component| matches!(component, std::path::Component::ParentDir))
    {
        return Err(MkoError::new(
            "repository_root_invalid",
            "repository destination cannot contain parent traversal",
        ));
    }
    let parent = absolute.parent().ok_or_else(|| {
        MkoError::new(
            "repository_root_invalid",
            "repository destination has no parent",
        )
    })?;
    let parent = fs::canonicalize(parent)
        .map_err(|error| MkoError::new("repository_root_invalid", error.to_string()))?;
    ensure_writable(&parent, "repository_root_invalid")?;
    let name = absolute.file_name().ok_or_else(|| {
        MkoError::new(
            "repository_root_invalid",
            "repository destination has no directory name",
        )
    })?;
    Ok(parent.join(name))
}

fn canonical_real_directory(path: &Path) -> Result<PathBuf, MkoError> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|error| MkoError::new("provider_root_invalid", error.to_string()))?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err(MkoError::new(
            "provider_root_invalid",
            "path must be a real directory",
        ));
    }
    fs::canonicalize(path)
        .map_err(|error| MkoError::new("provider_root_invalid", error.to_string()))
}

fn ensure_writable(path: &Path, code: &str) -> Result<(), MkoError> {
    if fs::metadata(path)
        .map_err(|error| MkoError::new(code, error.to_string()))?
        .permissions()
        .readonly()
    {
        Err(MkoError::new(code, "path must be writable"))
    } else {
        Ok(())
    }
}

fn reject_non_directory_if_present(path: &Path, code: &str) -> Result<(), MkoError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => Ok(()),
        Ok(_) => Err(MkoError::new(code, "managed path must be a real directory")),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(MkoError::new(code, error.to_string())),
    }
}

fn validate_expected_file(path: &Path, expected: &[u8], code: &str) -> Result<(), MkoError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_file() && !metadata.file_type().is_symlink() => {
            let bytes = fs::read(path).map_err(|error| MkoError::new(code, error.to_string()))?;
            if bytes == expected {
                Ok(())
            } else {
                Err(MkoError::new(code, "managed file bytes differ"))
            }
        }
        Ok(_) => Err(MkoError::new(code, "managed path must be a regular file")),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(MkoError::new(code, error.to_string())),
    }
}

fn observe_path(path: &Path, code: &str) -> Result<PathObservationV2, MkoError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {
            Ok(PathObservationV2::Directory)
        }
        Ok(metadata) if metadata.is_file() && !metadata.file_type().is_symlink() => {
            let bytes = fs::read(path).map_err(|error| MkoError::new(code, error.to_string()))?;
            Ok(PathObservationV2::File {
                size: bytes.len() as u64,
                digest: sha256_digest(&bytes),
            })
        }
        Ok(_) => Err(MkoError::new(
            code,
            "observed path must not be a link or special file",
        )),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            Ok(PathObservationV2::Missing)
        }
        Err(error) => Err(MkoError::new(code, error.to_string())),
    }
}

fn path_text(path: &Path) -> Result<String, MkoError> {
    path.to_str().map(str::to_owned).ok_or_else(|| {
        MkoError::new(
            "setup_path_invalid",
            "setup paths must be representable as Unicode",
        )
    })
}

fn ensure_plan_directories(
    platform: &dyn PlatformEnvironment,
) -> Result<PlanDirectories, MkoError> {
    let root = platform.config_home()?.join("mko/setup-plans");
    let root = ensure_private_directory_tree(&root)?;
    let open = ensure_private_directory(&root.join("open"))?;
    let consumed = ensure_private_directory(&root.join("consumed"))?;
    Ok(PlanDirectories { open, consumed })
}

fn ensure_private_directory_tree(path: &Path) -> Result<PathBuf, MkoError> {
    let mut missing = Vec::new();
    let mut current = path;
    loop {
        match fs::symlink_metadata(current) {
            Ok(metadata) => {
                if !metadata.is_dir() || metadata.file_type().is_symlink() {
                    return Err(MkoError::new(
                        "setup_plan_permissions_invalid",
                        "setup plan path must contain only real directories",
                    ));
                }
                break;
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                missing.push(current.to_path_buf());
                current = current.parent().ok_or_else(|| {
                    MkoError::new("setup_plan_write_failed", "setup plan path has no parent")
                })?;
            }
            Err(error) => {
                return Err(MkoError::new("setup_plan_write_failed", error.to_string()));
            }
        }
    }
    for directory in missing.iter().rev() {
        fs::create_dir(directory)
            .map_err(|error| MkoError::new("setup_plan_write_failed", error.to_string()))?;
        protect_private_directory(directory)?;
    }
    ensure_private_directory(path)
}

fn ensure_private_directory(path: &Path) -> Result<PathBuf, MkoError> {
    match fs::create_dir(path) {
        Ok(()) => protect_private_directory(path)?,
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(error) => {
            return Err(MkoError::new("setup_plan_write_failed", error.to_string()));
        }
    }
    let metadata = fs::symlink_metadata(path)
        .map_err(|error| MkoError::new("setup_plan_write_failed", error.to_string()))?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err(MkoError::new(
            "setup_plan_permissions_invalid",
            "setup plan path must contain only real directories",
        ));
    }
    ensure_private_directory_permissions(path)?;
    Ok(path.to_path_buf())
}

fn enforce_open_plan_bound(directory: &Path) -> Result<(), MkoError> {
    let deadline = Instant::now() + PLAN_SCAN_DEADLINE;
    let entries = fs::read_dir(directory)
        .map_err(|error| MkoError::new("setup_plan_scan_failed", error.to_string()))?;
    for (index, entry) in entries.enumerate() {
        if index + 1 >= MAX_OPEN_PLANS || Instant::now() >= deadline {
            return Err(MkoError::new(
                "setup_plan_scan_limit",
                "too many open setup plans; consume or expire existing plans before retrying",
            ));
        }
        let entry =
            entry.map_err(|error| MkoError::new("setup_plan_scan_failed", error.to_string()))?;
        require_private_regular_file(&entry.path())?;
    }
    Ok(())
}

fn cleanup_expired_open_plans(
    directories: &PlanDirectories,
    now: DateTime<Utc>,
) -> Result<(), MkoError> {
    let deadline = Instant::now() + PLAN_SCAN_DEADLINE;
    let mut entries = Vec::new();
    for (index, entry) in fs::read_dir(&directories.open)
        .map_err(|error| MkoError::new("setup_plan_scan_failed", error.to_string()))?
        .enumerate()
    {
        if index >= MAX_OPEN_PLANS || Instant::now() >= deadline {
            return Err(MkoError::new(
                "setup_plan_scan_limit",
                "open setup plan cleanup exceeded its bounded scan",
            ));
        }
        let entry =
            entry.map_err(|error| MkoError::new("setup_plan_scan_failed", error.to_string()))?;
        let name = entry.file_name().into_string().map_err(|_| {
            MkoError::new(
                "setup_plan_invalid",
                "open setup plan names must be valid Unicode",
            )
        })?;
        let plan_id = {
            let value = name.strip_suffix(".json").ok_or_else(|| {
                MkoError::new(
                    "setup_plan_invalid",
                    "the open setup plan directory contains an unmanaged entry",
                )
            })?;
            validate_plan_id(value)?;
            value.to_owned()
        };
        require_private_regular_file(&entry.path())?;
        entries.push((name, plan_id, entry.path()));
    }
    entries.sort_by(|left, right| left.0.cmp(&right.0));
    for (name, plan_id, open_path) in entries {
        if Instant::now() >= deadline {
            return Err(MkoError::new(
                "setup_plan_scan_limit",
                "open setup plan cleanup exceeded its bounded scan",
            ));
        }
        let stored = read_plan(&open_path)?;
        if stored.schema_version != 2 || stored.plan_id != plan_id {
            return Err(MkoError::new(
                "setup_plan_invalid",
                "the open setup plan identity is invalid",
            ));
        }
        if now >= stored.expires_at {
            consume_plan(&open_path, &directories.consumed.join(name))?;
        }
    }
    Ok(())
}

fn new_plan_id() -> Result<String, MkoError> {
    let mut random = [0_u8; 32];
    getrandom::fill(&mut random).map_err(|_| {
        MkoError::new(
            "setup_plan_random_failed",
            "secure randomness is unavailable for a setup plan capability",
        )
    })?;
    Ok(format!("{PLAN_PREFIX}{}", hex::encode(random)))
}

fn validate_plan_id(id: &str) -> Result<(), MkoError> {
    let suffix = id.strip_prefix(PLAN_PREFIX).ok_or_else(|| {
        MkoError::new(
            "setup_plan_id_invalid",
            "setup plan ID is not a Core-issued capability",
        )
    })?;
    if suffix.len() == 64
        && suffix
            .bytes()
            .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
    {
        Ok(())
    } else {
        Err(MkoError::new(
            "setup_plan_id_invalid",
            "setup plan ID is not a Core-issued capability",
        ))
    }
}

fn read_plan(path: &Path) -> Result<StoredSetupPlanV2, MkoError> {
    let bytes = read_private_regular(path)?;
    serde_json::from_slice(&bytes)
        .map_err(|error| MkoError::new("setup_plan_invalid", error.to_string()))
}

fn read_private_regular(path: &Path) -> Result<Vec<u8>, MkoError> {
    let mut options = OpenOptions::new();
    options.read(true);
    configure_nofollow(&mut options);
    let file = options.open(path).map_err(|error| {
        if error.kind() == std::io::ErrorKind::NotFound {
            MkoError::new(
                "setup_plan_not_found",
                "the machine-local setup plan does not exist on this device",
            )
        } else {
            MkoError::new("setup_plan_invalid", error.to_string())
        }
    })?;
    let metadata = file
        .metadata()
        .map_err(|error| MkoError::new("setup_plan_invalid", error.to_string()))?;
    if !metadata.is_file() || metadata.len() > MAX_PLAN_BYTES {
        return Err(MkoError::new(
            "setup_plan_invalid",
            "setup plan must be a bounded regular non-link file",
        ));
    }
    ensure_private_file_permissions(path)?;
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    file.take(MAX_PLAN_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| MkoError::new("setup_plan_invalid", error.to_string()))?;
    if bytes.len() as u64 > MAX_PLAN_BYTES {
        return Err(MkoError::new(
            "setup_plan_invalid",
            "setup plan exceeds its bounded input size",
        ));
    }
    Ok(bytes)
}

fn write_private_new(path: &Path, bytes: &[u8]) -> Result<(), MkoError> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    configure_private_create(&mut options);
    let mut file = options
        .open(path)
        .map_err(|error| MkoError::new("setup_plan_write_failed", error.to_string()))?;
    protect_private_file(&file)?;
    file.write_all(bytes)
        .and_then(|()| file.sync_all())
        .map_err(|error| MkoError::new("setup_plan_write_failed", error.to_string()))?;
    ensure_private_file_permissions(path)
}

fn consume_plan(open: &Path, consumed: &Path) -> Result<(), MkoError> {
    if path_exists_nofollow(consumed)? {
        return Err(consumed_error());
    }
    require_private_regular_file(open)?;
    fs::rename(open, consumed)
        .map_err(|error| MkoError::new("setup_plan_consume_failed", error.to_string()))?;
    require_private_regular_file(consumed)?;
    sync_parent_directory(consumed)
}

fn path_exists_nofollow(path: &Path) -> Result<bool, MkoError> {
    match fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(MkoError::new("setup_plan_invalid", error.to_string())),
    }
}

fn require_private_regular_file(path: &Path) -> Result<(), MkoError> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|error| MkoError::new("setup_plan_invalid", error.to_string()))?;
    if !metadata.is_file() || metadata.file_type().is_symlink() || metadata.len() > MAX_PLAN_BYTES {
        return Err(MkoError::new(
            "setup_plan_invalid",
            "setup plan must be a bounded regular non-link file",
        ));
    }
    ensure_private_file_permissions(path)
}

fn consumed_error() -> MkoError {
    MkoError::new(
        "setup_plan_consumed",
        "the single-use setup plan was already consumed",
    )
}

fn stale_error() -> MkoError {
    MkoError::new(
        "setup_plan_stale",
        "setup preconditions changed; create and approve a new plan",
    )
}

#[cfg(unix)]
fn protect_private_directory(path: &Path) -> Result<(), MkoError> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
        .map_err(|error| MkoError::new("setup_plan_permissions_invalid", error.to_string()))
}

#[cfg(windows)]
fn protect_private_directory(path: &Path) -> Result<(), MkoError> {
    mko_windows_acl::apply_owner_only_to_path(
        path,
        mko_windows_acl::Inheritance::ContainersAndObjects,
    )
    .map_err(|error| MkoError::new("setup_plan_permissions_invalid", error.to_string()))
}

#[cfg(not(any(unix, windows)))]
fn protect_private_directory(_path: &Path) -> Result<(), MkoError> {
    Err(MkoError::new(
        "setup_plan_permissions_unsupported",
        "owner-only setup plan storage is unsupported on this platform",
    ))
}

#[cfg(unix)]
fn ensure_private_directory_permissions(path: &Path) -> Result<(), MkoError> {
    use std::os::unix::fs::PermissionsExt;
    let mode = fs::symlink_metadata(path)
        .map_err(|error| MkoError::new("setup_plan_permissions_invalid", error.to_string()))?
        .permissions()
        .mode();
    if mode & 0o077 == 0 {
        Ok(())
    } else {
        Err(MkoError::new(
            "setup_plan_permissions_invalid",
            "setup plan directories must be owner-only",
        ))
    }
}

#[cfg(windows)]
fn ensure_private_directory_permissions(path: &Path) -> Result<(), MkoError> {
    validate_windows_acl(path)
}

#[cfg(not(any(unix, windows)))]
fn ensure_private_directory_permissions(_path: &Path) -> Result<(), MkoError> {
    Err(MkoError::new(
        "setup_plan_permissions_unsupported",
        "owner-only setup plan storage is unsupported on this platform",
    ))
}

#[cfg(unix)]
fn protect_private_file(file: &fs::File) -> Result<(), MkoError> {
    use std::os::unix::fs::PermissionsExt;
    file.set_permissions(fs::Permissions::from_mode(0o600))
        .map_err(|error| MkoError::new("setup_plan_permissions_invalid", error.to_string()))
}

#[cfg(windows)]
fn protect_private_file(file: &fs::File) -> Result<(), MkoError> {
    mko_windows_acl::apply_owner_only_to_file_handle(file, mko_windows_acl::Inheritance::None)
        .map_err(|error| MkoError::new("setup_plan_permissions_invalid", error.to_string()))
}

#[cfg(not(any(unix, windows)))]
fn protect_private_file(_file: &fs::File) -> Result<(), MkoError> {
    Err(MkoError::new(
        "setup_plan_permissions_unsupported",
        "owner-only setup plan storage is unsupported on this platform",
    ))
}

#[cfg(unix)]
fn ensure_private_file_permissions(path: &Path) -> Result<(), MkoError> {
    use std::os::unix::fs::PermissionsExt;
    let mode = fs::symlink_metadata(path)
        .map_err(|error| MkoError::new("setup_plan_permissions_invalid", error.to_string()))?
        .permissions()
        .mode();
    if mode & 0o077 == 0 {
        Ok(())
    } else {
        Err(MkoError::new(
            "setup_plan_permissions_invalid",
            "setup plan files must be owner-only",
        ))
    }
}

#[cfg(windows)]
fn ensure_private_file_permissions(path: &Path) -> Result<(), MkoError> {
    validate_windows_acl(path)
}

#[cfg(not(any(unix, windows)))]
fn ensure_private_file_permissions(_path: &Path) -> Result<(), MkoError> {
    Err(MkoError::new(
        "setup_plan_permissions_unsupported",
        "owner-only setup plan storage is unsupported on this platform",
    ))
}

#[cfg(windows)]
fn validate_windows_acl(path: &Path) -> Result<(), MkoError> {
    let acl = mko_windows_acl::inspect_path(path)
        .map_err(|error| MkoError::new("setup_plan_permissions_invalid", error.to_string()))?;
    if acl.is_owner_only_full_control() {
        Ok(())
    } else {
        Err(MkoError::new(
            "setup_plan_permissions_invalid",
            "setup plan ACL must grant full control only to the current user",
        ))
    }
}

#[cfg(unix)]
fn configure_private_create(options: &mut OpenOptions) {
    options.mode(0o600);
}

#[cfg(windows)]
fn configure_private_create(options: &mut OpenOptions) {
    options.security_qos_flags(0x0010_0000);
}

#[cfg(not(any(unix, windows)))]
fn configure_private_create(_options: &mut OpenOptions) {}

#[cfg(unix)]
fn configure_nofollow(options: &mut OpenOptions) {
    options.custom_flags(nix::libc::O_NOFOLLOW);
}

#[cfg(windows)]
fn configure_nofollow(options: &mut OpenOptions) {
    const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
    options.custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
}

#[cfg(not(any(unix, windows)))]
fn configure_nofollow(_options: &mut OpenOptions) {}

#[cfg(unix)]
fn sync_parent_directory(path: &Path) -> Result<(), MkoError> {
    let parent = path.parent().ok_or_else(|| {
        MkoError::new(
            "setup_plan_consume_failed",
            "setup plan path has no parent directory",
        )
    })?;
    fs::File::open(parent)
        .and_then(|directory| directory.sync_all())
        .map_err(|error| MkoError::new("setup_plan_consume_failed", error.to_string()))
}

#[cfg(not(unix))]
fn sync_parent_directory(_path: &Path) -> Result<(), MkoError> {
    Ok(())
}
