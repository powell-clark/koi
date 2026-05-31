//! End-to-end: scratch Downloads directory → scan → persist → approve → verify
//!
//! Exercises the full consent loop via the public koi-core API. No CLI, no
//! daemon — just the types and state layer working together. Guards against
//! regressions in the top-level flow even when individual unit tests pass.

use std::{fs, path::PathBuf};

use koi_core::{
    filing::{self, DownloadsMonitor, FileMonitor, Outcome, ProposedAction, ScanContext},
    state,
};

fn scratch(prefix: &str) -> PathBuf {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let p = std::env::temp_dir().join(format!("koi-e2e-{prefix}-{nanos:x}"));
    fs::create_dir_all(&p).unwrap();
    p
}

#[test]
fn scan_persist_approve_apply_verifies_file_moved() {
    let downloads = scratch("downloads");
    let documents = scratch("documents");
    let pdf = downloads.join("important.pdf");
    fs::write(&pdf, b"pretend this is a real PDF").unwrap();

    // 1. Scan produces proposals.
    let monitor = DownloadsMonitor::with_roots(downloads.clone(), documents.clone());
    let ctx = ScanContext::new_now();
    let proposals = monitor.scan(&ctx).expect("scan");
    assert_eq!(proposals.len(), 1, "one PDF = one proposal");
    let proposal = proposals.into_iter().next().unwrap();
    assert_eq!(proposal.monitor, "DownloadsMonitor");

    // 2. Persist to in-memory state.
    let conn = state::open_in_memory().expect("sqlite open");
    state::upsert_proposal(&conn, &proposal).expect("upsert");

    // Idempotent: re-upserting should not create duplicates.
    state::upsert_proposal(&conn, &proposal).expect("upsert repeat");
    let pending = state::pending_proposals(&conn).expect("pending");
    assert_eq!(pending.len(), 1);

    // 3. User approves — executor applies.
    let ProposedAction::Move { dest } = &proposal.action else {
        panic!("expected Move");
    };
    let outcome = filing::apply(&proposal.path, &proposal.action);
    assert!(
        matches!(outcome, Outcome::Applied),
        "executor outcome: {:?}",
        outcome
    );
    state::record_decision(
        &conn,
        &proposal.id,
        state::Decision::Approved,
        Some("e2e test"),
    )
    .expect("decision");

    // 4. Verify disk state: source gone, dest exists with original contents.
    assert!(!proposal.path.exists(), "source should be gone");
    assert!(dest.exists(), "destination should exist");
    assert_eq!(fs::read(dest).unwrap(), b"pretend this is a real PDF");

    // 5. Pending still shows the proposal until the app explicitly marks it applied.
    //    (CLI does this; state::record_decision alone doesn't flip pending for Approve.)
    //    Simulate the CLI's UPDATE here to complete the loop.
    conn.execute(
        "UPDATE proposals SET state = 'applied' WHERE id = ?1",
        rusqlite::params![proposal.id.0],
    )
    .expect("mark applied");
    let still_pending = state::pending_proposals(&conn).expect("pending after");
    assert!(
        still_pending.is_empty(),
        "nothing should be pending after approval + apply"
    );

    fs::remove_dir_all(&downloads).ok();
    fs::remove_dir_all(&documents).ok();
}

#[test]
fn rejection_records_signal_and_leaves_file_alone() {
    let downloads = scratch("downloads-reject");
    let documents = scratch("documents-reject");
    let file = downloads.join("keep-me.jpg");
    fs::write(&file, b"sacred photo").unwrap();

    let monitor = DownloadsMonitor::with_roots(downloads.clone(), documents.clone());
    let ctx = ScanContext::new_now();
    let proposals = monitor.scan(&ctx).expect("scan");
    let proposal = proposals.into_iter().next().unwrap();

    let conn = state::open_in_memory().expect("sqlite open");
    state::upsert_proposal(&conn, &proposal).expect("upsert");
    state::record_decision(
        &conn,
        &proposal.id,
        state::Decision::Rejected,
        Some("hands off"),
    )
    .expect("reject");

    // File must NOT move on rejection.
    assert!(file.exists(), "rejected file must remain in place");

    // Pending should be empty (rejection flips state).
    let pending = state::pending_proposals(&conn).expect("pending");
    assert!(pending.is_empty());

    fs::remove_dir_all(&downloads).ok();
    fs::remove_dir_all(&documents).ok();
}
