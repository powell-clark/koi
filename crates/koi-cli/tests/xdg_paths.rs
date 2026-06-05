//! Runtime state must resolve via XDG dirs, never from the source tree.
//!
//! ADR-0019: a published product cannot assume a `data/` dir in
//! its current working directory. `koi worklog` and `koi incidents` previously
//! read repo-relative `data/worklog.jsonl` / `data/incidents.jsonl`, walking up
//! from the cwd — so they only worked when standing in the checkout. These
//! tests pin the invariant: the resolved paths sit under the XDG data dir
//! (`directories::ProjectDirs` data_dir, equivalently `dirs::data_local_dir()/koi`
//! on Linux) and are independent of the current working directory.

use std::path::PathBuf;

use directories::ProjectDirs;
use koi_core::state;

/// The XDG data dir computed independently of the resolver under test.
fn expected_data_dir() -> PathBuf {
    ProjectDirs::from("com", "powellclark", "koi")
        .expect("user data directory")
        .data_dir()
        .to_path_buf()
}

/// A scratch working directory containing no `data/` dir, to prove the resolver
/// does not depend on the cwd.
fn scratch_cwd() -> PathBuf {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let p = std::env::temp_dir().join(format!("koi-xdg-{nanos:x}"));
    std::fs::create_dir_all(&p).unwrap();
    p
}

#[test]
fn worklog_path_resolves_under_xdg_data_dir() {
    let scratch = scratch_cwd();
    std::env::set_current_dir(&scratch).unwrap();

    let worklog = state::default_data_dir()
        .expect("resolve data dir")
        .join("worklog.jsonl");

    assert!(worklog.is_absolute(), "resolved path must be absolute");
    assert_eq!(worklog.parent().unwrap(), expected_data_dir().as_path());
    assert_eq!(worklog.file_name().unwrap(), "worklog.jsonl");
    assert!(
        !worklog.starts_with(&scratch),
        "resolved path must not depend on the cwd ({})",
        worklog.display()
    );
}

#[test]
fn incidents_path_resolves_under_xdg_data_dir() {
    let scratch = scratch_cwd();
    std::env::set_current_dir(&scratch).unwrap();

    let incidents = state::default_data_dir()
        .expect("resolve data dir")
        .join("incidents.jsonl");

    assert!(incidents.is_absolute(), "resolved path must be absolute");
    assert_eq!(incidents.parent().unwrap(), expected_data_dir().as_path());
    assert_eq!(incidents.file_name().unwrap(), "incidents.jsonl");
    assert!(
        !incidents.starts_with(&scratch),
        "resolved path must not depend on the cwd ({})",
        incidents.display()
    );
}
