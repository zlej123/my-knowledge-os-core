use mko_core::{
    attempt_v2::{
        PreparationOutcomeV2, latest_preparation_attempt_v2, latest_preparation_attempts_v2,
        record_preparation_attempt_v2,
    },
    clock::SystemClock,
    scaffold_v2::scaffold_personal_kb_v2,
};
use tempfile::tempdir;

fn asset(fill: char) -> String {
    format!("personal-asset-{}", String::from(fill).repeat(64))
}

// Home used to ask "what was the latest attempt" once per unfinished Asset,
// and each question re-read and re-parsed the whole attempts directory. With
// five hundred stalled posts and a thousand attempt files on disk that was
// measured at seven to forty seconds per `mko`, with nothing on screen to
// say why. One pass must answer for every Asset at once, and must agree
// with the per-Asset answer it replaces.
#[test]
fn one_pass_yields_the_latest_attempt_for_every_asset() {
    let root = tempdir().unwrap();
    let repository = root.path().join("kb");
    scaffold_personal_kb_v2(&repository).unwrap();
    let clock = SystemClock;

    // Asset A: failed, then prepared — the later outcome wins.
    record_preparation_attempt_v2(
        &repository,
        &asset('a'),
        PreparationOutcomeV2::Failed,
        Some("pdf_text_unreadable"),
        &clock,
    )
    .unwrap();
    std::thread::sleep(std::time::Duration::from_millis(5));
    record_preparation_attempt_v2(
        &repository,
        &asset('a'),
        PreparationOutcomeV2::Prepared,
        None,
        &clock,
    )
    .unwrap();
    // Asset B: one failure, still the latest.
    record_preparation_attempt_v2(
        &repository,
        &asset('b'),
        PreparationOutcomeV2::Failed,
        Some("hydration_confirmation_required"),
        &clock,
    )
    .unwrap();

    let latest = latest_preparation_attempts_v2(&repository).unwrap();

    assert_eq!(latest.len(), 2, "one entry per Asset that has attempts");
    assert_eq!(latest[&asset('a')].outcome, PreparationOutcomeV2::Prepared);
    assert_eq!(latest[&asset('a')].code, None);
    assert_eq!(latest[&asset('b')].outcome, PreparationOutcomeV2::Failed);
    assert_eq!(
        latest[&asset('b')].code.as_deref(),
        Some("hydration_confirmation_required")
    );
    assert!(
        !latest.contains_key(&asset('c')),
        "an Asset with no attempts has no entry, which home reads as not attempted"
    );

    // The per-Asset form is now a view over the same pass and must agree.
    for id in [asset('a'), asset('b')] {
        assert_eq!(
            latest_preparation_attempt_v2(&repository, &id)
                .unwrap()
                .as_ref(),
            latest.get(&id)
        );
    }
    assert_eq!(
        latest_preparation_attempt_v2(&repository, &asset('c')).unwrap(),
        None
    );
}

// A knowledge base that has never recorded an attempt has no directory yet;
// that is an empty answer, not an error.
#[test]
fn a_knowledge_base_with_no_attempts_answers_empty() {
    let root = tempdir().unwrap();
    let repository = root.path().join("kb");
    scaffold_personal_kb_v2(&repository).unwrap();
    std::fs::remove_dir_all(repository.join("assets/attempts")).ok();

    assert!(
        latest_preparation_attempts_v2(&repository)
            .unwrap()
            .is_empty()
    );
}
