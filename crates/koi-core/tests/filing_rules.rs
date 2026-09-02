//! One file per seed rule through a real scan (TASK-KOI246 AC-5).
//!
//! The unit tests in `filing::rules` prove the table matches. This proves the
//! monitor actually files by it: a fixture directory holding one file per seed
//! rule, scanned through the public API, must propose each to its taxonomy
//! destination rather than to an extension bucket.

use std::{fs, path::PathBuf};

use koi_core::filing::{DownloadsMonitor, FileMonitor, ProposedAction, ScanContext};

fn scratch(prefix: &str) -> PathBuf {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let p = std::env::temp_dir().join(format!("koi-rules-{prefix}-{nanos:x}"));
    fs::create_dir_all(&p).unwrap();
    p
}

/// Every flagged file from TASK-KOI214, plus the loose set measured
/// 2026-09-02, with the destination each must reach.
const CASES: &[(&str, &str)] = &[
    ("Monzo_bank_statement_2026-01.pdf", "Finance/Statements"),
    ("StarlingCertifiedStatement_2026.pdf", "Finance/Statements"),
    ("Invoice TB-EPC-031.pdf", "Finance/Invoices"),
    ("invoice-PCL-2026-001.pdf", "Finance/Invoices"),
    ("Receipt-2613-6471.pdf", "Finance/Receipts"),
    ("patient-statement-emmanuel-powell-clark.pdf", "Health"),
    ("EPC Passport.pdf", "Personal/Identity"),
    (
        "financials-and-companies-house-20251031T175423Z-1-001.zip",
        "Finance/Tax-and-Companies-House",
    ),
    ("Inter-Regular.ttf", "Fonts"),
    ("ventoy_1.0.99_amd64.deb", "Software"),
];

#[test]
fn every_seed_rule_files_to_its_taxonomy_destination() {
    let downloads = scratch("dl");
    let documents = scratch("docs");
    for (name, _) in CASES {
        fs::write(downloads.join(name), b"fixture").unwrap();
    }

    let monitor = DownloadsMonitor::with_roots(downloads.clone(), documents.clone());
    let proposals = monitor
        .scan(&ScanContext::new_now_with_roots(std::slice::from_ref(
            &downloads,
        )))
        .expect("scan");

    for (name, expected_dest) in CASES {
        let p = proposals
            .iter()
            .find(|p| p.path.file_name().unwrap() == *name)
            .unwrap_or_else(|| panic!("{name} produced no proposal"));
        let ProposedAction::Move { dest } = &p.action else {
            panic!("{name} should be a Move");
        };
        let expected = documents.join(expected_dest).join(name);
        assert_eq!(
            dest,
            &expected,
            "{name} filed to {} but belongs in {expected_dest}",
            dest.display()
        );
    }

    fs::remove_dir_all(&downloads).ok();
    fs::remove_dir_all(&documents).ok();
}

#[test]
fn an_unmatched_file_still_reaches_its_extension_bucket() {
    // The card's second pre-mortem failure: moving rules into a table must not
    // silently disable the fallback for files no rule claims.
    let downloads = scratch("dl-fallback");
    let documents = scratch("docs-fallback");
    fs::write(downloads.join("holiday-photo-2019.jpg"), b"fixture").unwrap();

    let monitor = DownloadsMonitor::with_roots(downloads.clone(), documents.clone());
    let proposals = monitor
        .scan(&ScanContext::new_now_with_roots(std::slice::from_ref(
            &downloads,
        )))
        .expect("scan");

    let p = proposals.first().expect("a proposal for the loose photo");
    let ProposedAction::Move { dest } = &p.action else {
        panic!("expected a Move");
    };
    assert_eq!(
        dest,
        &documents.join("Images").join("holiday-photo-2019.jpg")
    );

    fs::remove_dir_all(&downloads).ok();
    fs::remove_dir_all(&documents).ok();
}

#[test]
fn content_bearing_proposals_stay_at_human_tier() {
    // TASK-KOI229's guard: a bank statement must not become sweepable by
    // `approve --all` just because a rule now names its destination.
    use koi_core::filing::AutonomyTier;
    let downloads = scratch("dl-tier");
    let documents = scratch("docs-tier");
    fs::write(downloads.join("Monzo_bank_statement_2026-01.pdf"), b"x").unwrap();

    let monitor = DownloadsMonitor::with_roots(downloads.clone(), documents.clone());
    let proposals = monitor
        .scan(&ScanContext::new_now_with_roots(std::slice::from_ref(
            &downloads,
        )))
        .expect("scan");

    assert_eq!(proposals[0].autonomy_tier, AutonomyTier::Human);

    fs::remove_dir_all(&downloads).ok();
    fs::remove_dir_all(&documents).ok();
}

/// The AC-7 deviation, pinned as behaviour so it cannot regress silently.
///
/// A learned extension-keyed destination must NOT defeat a content rule. If it
/// did, one stale `.pdf` decision would send every bank and patient statement
/// back to a flat bucket.
#[test]
fn an_extension_keyed_learned_destination_does_not_defeat_a_content_rule() {
    use koi_core::filing::Classifier;

    struct AlwaysPdfBucket(PathBuf);
    impl Classifier for AlwaysPdfBucket {
        fn suggest(&self, _monitor: &str, suffix: &str) -> Option<(PathBuf, f32)> {
            (suffix == ".pdf").then(|| (self.0.join("PDFs"), 0.99))
        }
    }

    let downloads = scratch("dl-learned");
    let documents = scratch("docs-learned");
    fs::write(downloads.join("patient-statement-epc.pdf"), b"x").unwrap();
    fs::write(downloads.join("meeting-notes.pdf"), b"x").unwrap();

    let ctx = ScanContext::new_now_with_roots(std::slice::from_ref(&downloads))
        .with_classifier(Box::new(AlwaysPdfBucket(documents.clone())));
    let monitor = DownloadsMonitor::with_roots(downloads.clone(), documents.clone());
    let proposals = monitor.scan(&ctx).expect("scan");

    let dest_of = |name: &str| -> PathBuf {
        let p = proposals
            .iter()
            .find(|p| p.path.file_name().unwrap() == name)
            .unwrap_or_else(|| panic!("no proposal for {name}"));
        match &p.action {
            ProposedAction::Move { dest } => dest.clone(),
            other => panic!("expected a Move, got {other:?}"),
        }
    };

    // The rule wins where it has an opinion...
    assert_eq!(
        dest_of("patient-statement-epc.pdf"),
        documents.join("Health").join("patient-statement-epc.pdf"),
        "a content rule must outrank an extension-keyed learned destination"
    );
    // ...and the learner still wins everywhere it is the only opinion.
    assert_eq!(
        dest_of("meeting-notes.pdf"),
        documents.join("PDFs").join("meeting-notes.pdf"),
        "the learner must keep precedence for files no rule claims"
    );

    fs::remove_dir_all(&downloads).ok();
    fs::remove_dir_all(&documents).ok();
}
