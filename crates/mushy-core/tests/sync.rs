use mushy_core::{engine, model::*, protocol, scan, store::Store};
use std::{collections::BTreeMap, fs};
use tempfile::TempDir;
struct Device {
    _tmp: TempDir,
    store: Store,
    root: Root,
}
impl Device {
    fn new() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("files");
        fs::create_dir(&path).unwrap();
        let store = Store::open(&tmp.path().join("state")).unwrap();
        let root = Root {
            id: uuid::Uuid::new_v4().to_string(),
            group: "test-group".into(),
            path: path.to_str().unwrap().into(),
            single_file: false,
            excludes: vec![],
            paused: false,
        };
        store.save_root(&root).unwrap();
        Self {
            _tmp: tmp,
            store,
            root,
        }
    }
    fn write(&self, path: &str, text: &str) {
        let p = std::path::Path::new(&self.root.path).join(path);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, text).unwrap();
    }
    fn read(&self, path: &str) -> String {
        fs::read_to_string(std::path::Path::new(&self.root.path).join(path)).unwrap()
    }
    fn observe(&self) {
        engine::observe(&self.store, &self.root).unwrap();
    }
    fn events(&self) -> Vec<Change> {
        self.store.changes(&self.root.group).unwrap()
    }
    fn merge(&self, other: &Device) {
        self.store
            .import(&self.root.group, &other.events())
            .unwrap();
        for (hash, _) in engine::missing_objects(&self.store, &self.root.group).unwrap() {
            let from = other.store.object(&hash).unwrap();
            if from.exists() {
                fs::copy(from, self.store.object(&hash).unwrap()).unwrap();
            }
        }
        engine::apply(&self.store, &self.root).unwrap();
    }
}
fn change(device: &str, sequence: u64, ts: i64, context: Clock) -> Change {
    Change {
        id: format!("{device}:{sequence}"),
        group: "g".into(),
        device: device.into(),
        sequence,
        context,
        path: "a".into(),
        entry: Entry {
            kind: Kind::Deleted,
            modified_ns: ts,
        },
    }
}
#[test]
fn concurrent_winner_is_order_independent() {
    let a = change("a", 1, 100, Clock::new());
    let b = change("b", 1, 100, Clock::new());
    assert_eq!(winner(&[a.clone(), b.clone()]).unwrap().id, "b:1");
    assert_eq!(winner(&[b, a]).unwrap().id, "b:1");
}
#[test]
fn causality_beats_clock_skew() {
    let a = change("a", 1, 999, Clock::new());
    let b = change("b", 1, 1, BTreeMap::from([("a".into(), 1)]));
    assert_eq!(winner(&[a, b]).unwrap().id, "b:1");
}
#[test]
fn copy_edits_deletes_and_restore() {
    let a = Device::new();
    let b = Device::new();
    a.write("nested/a.txt", "first");
    a.observe();
    b.merge(&a);
    assert_eq!(b.read("nested/a.txt"), "first");
    let original = a
        .events()
        .into_iter()
        .find(|c| c.path == "nested/a.txt")
        .unwrap();
    b.write("nested/a.txt", "second");
    b.observe();
    a.merge(&b);
    assert_eq!(a.read("nested/a.txt"), "second");
    fs::remove_file(std::path::Path::new(&b.root.path).join("nested/a.txt")).unwrap();
    b.observe();
    a.merge(&b);
    assert!(
        !std::path::Path::new(&a.root.path)
            .join("nested/a.txt")
            .exists()
    );
    engine::restore(&a.store, &a.root, "nested/a.txt", &original.id).unwrap();
    assert_eq!(a.read("nested/a.txt"), "first");
    b.merge(&a);
    assert_eq!(b.read("nested/a.txt"), "first");
}
#[test]
fn diamond_relay_does_not_create_new_edits() {
    let a = Device::new();
    let b = Device::new();
    let c = Device::new();
    let d = Device::new();
    a.write("x", "v1");
    a.observe();
    b.merge(&a);
    c.merge(&a);
    d.merge(&b);
    d.merge(&c);
    a.merge(&d);
    for dev in [&a, &b, &c, &d] {
        dev.observe();
        assert_eq!(dev.events().len(), 1);
        assert_eq!(dev.read("x"), "v1");
    }
}
#[test]
fn partitioned_edits_converge_and_loser_is_retained() {
    let a = Device::new();
    let b = Device::new();
    a.write("x", "base");
    a.observe();
    b.merge(&a);
    a.write("x", "A edit");
    a.observe();
    b.write("x", "B edit");
    b.observe();
    let a_events = a.events();
    let b_events = b.events();
    a.store.import(&a.root.group, &b_events).unwrap();
    b.store.import(&b.root.group, &a_events).unwrap();
    for c in a.events() {
        if let Some(h) = c.entry.kind.hash() {
            let pa = a.store.object(h).unwrap();
            let pb = b.store.object(h).unwrap();
            if pa.exists() && !pb.exists() {
                fs::copy(&pa, &pb).unwrap();
            }
            if pb.exists() && !pa.exists() {
                fs::copy(&pb, &pa).unwrap();
            }
        }
    }
    engine::apply(&a.store, &a.root).unwrap();
    engine::apply(&b.store, &b.root).unwrap();
    assert_eq!(a.read("x"), b.read("x"));
    a.observe();
    b.observe();
    assert_eq!(a.events().len(), 3);
    assert_eq!(b.events().len(), 3);
    assert_eq!(
        a.store.heads(&a.root.group).unwrap()["x"].id,
        b.store.heads(&b.root.group).unwrap()["x"].id
    );
}
#[test]
fn duplicate_events_are_idempotent_and_immutable() {
    let a = Device::new();
    a.write("x", "text");
    a.observe();
    let events = a.events();
    a.store.import(&a.root.group, &events).unwrap();
    assert_eq!(a.events().len(), 1);
    let mut forged = events;
    forged[0].entry.modified_ns += 1;
    assert!(a.store.import(&a.root.group, &forged).is_err());
}
#[test]
fn exact_limit_accepted_oversized_rejected() {
    let a = Device::new();
    let mut l = a.store.limits().unwrap();
    l.bytes = 4;
    l.file_bytes = 4;
    l.files = 1;
    a.store.set_limits(&l).unwrap();
    a.write("x", "1234");
    a.observe();
    a.write("x", "12345");
    assert!(
        engine::observe(&a.store, &a.root)
            .unwrap_err()
            .to_string()
            .contains("limit exceeded")
    );
    assert_eq!(a.events().len(), 1);
}
#[test]
fn file_count_is_checked_even_for_empty_files() {
    let a = Device::new();
    let mut l = a.store.limits().unwrap();
    l.files = 1;
    a.store.set_limits(&l).unwrap();
    a.write("a", "");
    a.write("b", "");
    assert!(scan::scan(&a.store, &a.root, true).is_err());
}
#[test]
fn exclusion_prevents_large_file_from_counting() {
    let mut a = Device::new();
    a.root.excludes = vec!["build/**".into()];
    a.store.save_root(&a.root).unwrap();
    a.write("build/huge", "large");
    a.write("ok", "x");
    let mut l = a.store.limits().unwrap();
    l.file_bytes = 1;
    a.store.set_limits(&l).unwrap();
    let s = scan::scan(&a.store, &a.root, true).unwrap();
    assert!(s.entries.contains_key("ok"));
    assert!(!s.entries.contains_key("build/huge"));
}
#[test]
fn recovery_budget_blocks_overwrite() {
    let a = Device::new();
    let b = Device::new();
    a.write("x", "first");
    a.observe();
    b.merge(&a);
    let mut l = a.store.limits().unwrap();
    l.recovery_bytes = 1;
    a.store.set_limits(&l).unwrap();
    b.write("x", "second");
    b.observe();
    a.store.import(&a.root.group, &b.events()).unwrap();
    for (h, _) in engine::missing_objects(&a.store, &a.root.group).unwrap() {
        fs::copy(b.store.object(&h).unwrap(), a.store.object(&h).unwrap()).unwrap();
    }
    assert!(
        engine::apply(&a.store, &a.root)
            .unwrap_err()
            .to_string()
            .contains("recovery budget")
    );
    assert_eq!(a.read("x"), "first");
}
#[test]
fn missing_directory_does_not_generate_deletions() {
    let a = Device::new();
    a.write("x", "keep");
    a.observe();
    fs::rename(&a.root.path, format!("{}-moved", a.root.path)).unwrap();
    assert!(engine::observe(&a.store, &a.root).is_err());
    assert_eq!(a.events().len(), 1);
}
#[test]
fn stale_remote_snapshot_cannot_overwrite_new_local_edit() {
    let a = Device::new();
    let b = Device::new();
    a.write("x", "base");
    a.observe();
    b.merge(&a);
    b.write("x", "remote");
    b.observe();
    a.store.import(&a.root.group, &b.events()).unwrap();
    for (h, _) in engine::missing_objects(&a.store, &a.root.group).unwrap() {
        fs::copy(b.store.object(&h).unwrap(), a.store.object(&h).unwrap()).unwrap();
    }
    a.write("x", "unsent local");
    assert!(engine::apply(&a.store, &a.root).is_err());
    assert_eq!(a.read("x"), "unsent local");
}
#[test]
fn pending_commit_is_recovered_without_new_change() {
    let a = Device::new();
    a.write("x", "base");
    a.observe();
    let old = a.events()[0].clone();
    a.store.intent(&a.root.id, &old).unwrap();
    a.observe();
    assert!(a.store.pending(&a.root.id).unwrap().is_empty());
    assert_eq!(a.events().len(), 1);
}
#[test]
fn invalid_paths_and_symlink_escape_rejected() {
    for p in [
        "../x",
        "/x",
        "a/../x",
        "a//x",
        "a\\x",
        "C:/x",
        ".mushy-stage-a",
    ] {
        assert!(validate_path(p).is_err(), "{p}");
    }
    assert!(validate_target("a/link", "../../etc").is_err());
    assert!(validate_target("a/link", "/etc").is_err());
    assert!(validate_target("a/link", "../file").is_ok());
}
#[cfg(unix)]
#[test]
fn internal_symlink_is_preserved_without_traversal() {
    let a = Device::new();
    let b = Device::new();
    a.write("target", "value");
    std::os::unix::fs::symlink("target", std::path::Path::new(&a.root.path).join("link")).unwrap();
    a.observe();
    b.merge(&a);
    assert_eq!(
        fs::read_link(std::path::Path::new(&b.root.path).join("link")).unwrap(),
        std::path::Path::new("target")
    );
    b.observe();
    assert_eq!(b.events().len(), 2);
}
#[cfg(unix)]
#[test]
fn outside_symlink_is_rejected() {
    let a = Device::new();
    std::os::unix::fs::symlink("../secret", std::path::Path::new(&a.root.path).join("link"))
        .unwrap();
    assert!(scan::scan(&a.store, &a.root, true).is_err());
}
#[test]
fn portable_names_and_case_collisions_rejected() {
    for p in ["CON", "aux.txt", "LPT1.md", "trailing.", "bad?name"] {
        assert!(scan::portable_name(p).is_err());
    }
    #[cfg(target_os = "linux")]
    {
        let a = Device::new();
        a.write("Readme", "a");
        a.write("README", "b");
        assert!(scan::scan(&a.store, &a.root, true).is_err());
    }
}
#[test]
fn overlapping_roots_and_duplicate_group_rejected() {
    let a = Device::new();
    let mut r = a.root.clone();
    r.id = "other".into();
    r.path = format!("{}/nested", r.path);
    r.group = "other-group".into();
    assert!(a.store.save_root(&r).is_err());
    r.path = "/different".into();
    r.group = a.root.group.clone();
    assert!(a.store.save_root(&r).is_err());
}
#[test]
fn protocol_limits_allocation_and_roundtrips() {
    let mut bytes = Vec::new();
    protocol::write(
        &mut bytes,
        &protocol::Request::Snapshot {
            known: Clock::new(),
        },
    )
    .unwrap();
    let _: protocol::Request = protocol::read(&mut bytes.as_slice()).unwrap();
    let bad = u32::MAX.to_be_bytes();
    assert!(protocol::read::<protocol::Request>(&mut bad.as_slice()).is_err());
}
#[test]
fn device_totals_count_roots_once() {
    let a = Device::new();
    a.write("x", "1234");
    a.observe();
    let mut l = a.store.limits().unwrap();
    l.device_bytes = 5;
    a.store.set_limits(&l).unwrap();
    assert!(
        a.store
            .check_device(
                &a.root,
                &Usage {
                    bytes: 4,
                    files: 1,
                    directories: 0,
                    ..Usage::default()
                }
            )
            .is_ok()
    );
    let mut other = a.root.clone();
    other.id = "different".into();
    assert!(
        a.store
            .check_device(
                &other,
                &Usage {
                    bytes: 4,
                    files: 1,
                    directories: 0,
                    ..Usage::default()
                }
            )
            .is_err()
    );
}
#[test]
fn single_file_link_can_be_deleted_and_restored() {
    let mut a = Device::new();
    a.root.path = std::path::Path::new(&a.root.path)
        .join("only.txt")
        .to_str()
        .unwrap()
        .into();
    a.root.single_file = true;
    a.store.save_root(&a.root).unwrap();
    fs::write(&a.root.path, "one").unwrap();
    a.observe();
    let first = a.events()[0].clone();
    fs::remove_file(&a.root.path).unwrap();
    a.observe();
    assert_eq!(
        a.store.heads(&a.root.group).unwrap()["file"].entry.kind,
        Kind::Deleted
    );
    engine::restore(&a.store, &a.root, "file", &first.id).unwrap();
    assert_eq!(fs::read_to_string(&a.root.path).unwrap(), "one");
}

#[test]
fn concurrent_child_survives_parent_deletion() {
    let a = Device::new();
    let b = Device::new();
    a.write("dir/old", "base");
    a.observe();
    b.merge(&a);
    fs::remove_dir_all(std::path::Path::new(&a.root.path).join("dir")).unwrap();
    a.observe();
    b.write("dir/new", "concurrent child");
    b.observe();
    b.merge(&a);
    a.merge(&b);
    assert_eq!(a.read("dir/new"), "concurrent child");
    assert_eq!(b.read("dir/new"), "concurrent child");
    assert!(!std::path::Path::new(&a.root.path).join("dir/old").exists());
    a.observe();
    b.observe();
    a.merge(&b);
    b.merge(&a);
    assert_eq!(
        a.store.heads(&a.root.group).unwrap()["dir"].entry.kind,
        Kind::Directory
    );
}
#[test]
fn deleted_directory_with_excluded_files_is_retained() {
    let mut a = Device::new();
    let mut b = Device::new();
    a.root.excludes = vec!["dir/cache".into()];
    b.root.excludes = a.root.excludes.clone();
    a.store.save_root(&a.root).unwrap();
    b.store.save_root(&b.root).unwrap();
    a.write("dir/tracked", "base");
    a.observe();
    b.merge(&a);
    b.write("dir/cache", "private");
    b.observe();
    fs::remove_dir_all(std::path::Path::new(&a.root.path).join("dir")).unwrap();
    a.observe();
    b.merge(&a);
    assert_eq!(b.read("dir/cache"), "private");
    assert!(
        !std::path::Path::new(&b.root.path)
            .join("dir/tracked")
            .exists()
    );
}
#[cfg(unix)]
#[test]
fn swapped_parent_symlink_cannot_redirect_a_write() {
    let a = Device::new();
    let b = Device::new();
    a.write("dir/x", "old");
    a.observe();
    b.merge(&a);
    a.write("dir/x", "new");
    a.observe();
    b.store.import(&b.root.group, &a.events()).unwrap();
    for (h, _) in engine::missing_objects(&b.store, &b.root.group).unwrap() {
        fs::copy(a.store.object(&h).unwrap(), b.store.object(&h).unwrap()).unwrap();
    }
    let outside = tempfile::tempdir().unwrap();
    fs::write(outside.path().join("x"), "secret").unwrap();
    fs::remove_dir_all(std::path::Path::new(&b.root.path).join("dir")).unwrap();
    std::os::unix::fs::symlink(
        outside.path(),
        std::path::Path::new(&b.root.path).join("dir"),
    )
    .unwrap();
    assert!(engine::apply(&b.store, &b.root).is_err());
    assert_eq!(
        fs::read_to_string(outside.path().join("x")).unwrap(),
        "secret"
    );
}
#[test]
fn tombstones_survive_recovery_expiry_and_stale_peer() {
    let a = Device::new();
    let b = Device::new();
    a.write("x", "old");
    a.observe();
    b.merge(&a);
    fs::remove_file(std::path::Path::new(&a.root.path).join("x")).unwrap();
    a.observe();
    a.store
        .db
        .execute("UPDATE blobs SET expires=0", [])
        .unwrap();
    a.store.gc().unwrap();
    a.store.import(&a.root.group, &b.events()).unwrap();
    engine::apply(&a.store, &a.root).unwrap();
    b.merge(&a);
    assert!(!std::path::Path::new(&b.root.path).join("x").exists());
    assert_eq!(a.events().len(), 2);
}

#[test]
fn interrupted_filesystem_commit_is_recognized_after_restart() {
    let a = Device::new();
    let b = Device::new();
    a.write("x", "old");
    a.observe();
    b.merge(&a);
    a.write("x", "new");
    a.observe();
    b.store.import(&b.root.group, &a.events()).unwrap();
    let incoming = b.store.heads(&b.root.group).unwrap()["x"].clone();
    b.store.intent(&b.root.id, &incoming).unwrap();
    // Simulate rename completed, process exited before applied-state commit.
    b.write("x", "new");
    b.observe();
    assert_eq!(b.events().len(), 2);
    assert!(b.store.pending(&b.root.id).unwrap().is_empty());
    assert_eq!(b.store.applied(&b.root.id).unwrap()["x"].id, incoming.id);
}
#[test]
fn failed_filesystem_commit_retries_without_discarding_local_edits() {
    let a = Device::new();
    let b = Device::new();
    a.write("x", "old");
    a.observe();
    b.merge(&a);
    a.write("x", "incoming");
    a.observe();
    b.store.import(&b.root.group, &a.events()).unwrap();
    let incoming = b.store.heads(&b.root.group).unwrap()["x"].clone();
    b.store.intent(&b.root.id, &incoming).unwrap();
    b.write("x", "local edit after crash");
    b.observe();
    assert_eq!(b.events().len(), 3);
    assert_eq!(b.read("x"), "local edit after crash");
    assert_eq!(
        b.store.heads(&b.root.group).unwrap()["x"].entry.kind,
        b.store.applied(&b.root.id).unwrap()["x"].entry.kind
    );
}
#[test]
fn unlink_retains_accessible_history_and_content() {
    let a = Device::new();
    a.write("x", "keep");
    a.observe();
    let hash = a.events()[0].entry.kind.hash().unwrap().to_owned();
    a.store.unlink(&a.root).unwrap();
    assert!(a.store.roots().unwrap().is_empty());
    assert_eq!(a.store.archived_roots().unwrap().len(), 1);
    assert!(a.store.object(&hash).unwrap().exists());
    assert_eq!(a.read("x"), "keep");
}
#[test]
fn metadata_limit_rolls_back_import_batch() {
    let a = Device::new();
    let b = Device::new();
    a.write("a", "a");
    a.write("b", "b");
    a.observe();
    let mut limits = b.store.limits().unwrap();
    limits.metadata_bytes = 1;
    b.store.set_limits(&limits).unwrap();
    assert!(b.store.import(&b.root.group, &a.events()).is_err());
    assert!(b.events().is_empty());
}

#[test]
fn unicode_filenames_and_symlink_targets_roundtrip() {
    let a = Device::new();
    let b = Device::new();
    a.write("café/東京.txt", "Unicode content");
    a.observe();
    b.merge(&a);
    assert_eq!(b.read("café/東京.txt"), "Unicode content");
    b.observe();
    assert_eq!(a.events().len(), b.events().len());
}
#[test]
fn normalized_wire_paths_are_required() {
    assert!(validate_path("cafe\u{301}.txt").is_err());
    assert!(validate_path("café.txt").is_ok());
}

#[test]
fn recovery_lookup_handles_missing_parent_directories() {
    let tmp = tempfile::tempdir().unwrap();
    let missing = tmp.path().join("deleted/parent/file.txt");
    assert_eq!(
        scan::absolute(&missing).unwrap(),
        tmp.path()
            .canonicalize()
            .unwrap()
            .join("deleted/parent/file.txt")
    );
}

#[test]
fn causal_frontier_is_independent_of_delivery_order() {
    let a1 = change("a", 1, 100, Clock::new());
    let b1 = change("b", 1, 1, BTreeMap::from([("a".into(), 1)]));
    let a2 = change(
        "a",
        2,
        2,
        BTreeMap::from([("a".into(), 1), ("b".into(), 1)]),
    );
    let c1 = change("c", 1, 50, Clock::new());
    let events = [a1, b1, a2, c1];
    for a in 0..4 {
        for b in 0..4 {
            for c in 0..4 {
                for d in 0..4 {
                    let mut indices = vec![a, b, c, d];
                    indices.sort();
                    if indices != [0, 1, 2, 3] {
                        continue;
                    }
                    let ordered = [
                        events[a].clone(),
                        events[b].clone(),
                        events[c].clone(),
                        events[d].clone(),
                    ];
                    assert_eq!(winner(&ordered).unwrap().id, "c:1");
                }
            }
        }
    }
}

#[test]
fn replaced_root_cannot_masquerade_as_mass_deletion() {
    let a = Device::new();
    a.write("x", "keep");
    a.observe();
    let old = std::path::Path::new(&a.root.path).with_extension("old");
    fs::rename(&a.root.path, &old).unwrap();
    fs::create_dir(&a.root.path).unwrap();
    assert!(
        engine::observe(&a.store, &a.root)
            .unwrap_err()
            .to_string()
            .contains("identity changed")
    );
    assert_eq!(a.events().len(), 1);
}

#[test]
fn replaced_file_root_parent_does_not_generate_a_deletion() {
    let mut a = Device::new();
    let parent = std::path::PathBuf::from(&a.root.path);
    a.root.path = parent.join("only.txt").to_str().unwrap().into();
    a.root.single_file = true;
    a.store.save_root(&a.root).unwrap();
    fs::write(&a.root.path, "keep").unwrap();
    a.observe();
    fs::rename(&parent, parent.with_extension("old")).unwrap();
    fs::create_dir(&parent).unwrap();
    assert!(engine::observe(&a.store, &a.root).is_err());
    assert_eq!(a.events().len(), 1);
}

#[test]
fn unlink_survives_concurrent_background_status_updates() {
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    };
    let a = Device::new();
    a.write("x", "recoverable");
    let background = Store::open(&a.store.dir).unwrap();
    let stop = Arc::new(AtomicBool::new(false));
    let finished = stop.clone();
    let (ready, started) = mpsc::channel();
    let worker = std::thread::spawn(move || {
        background.status("background", "syncing", "").unwrap();
        ready.send(()).unwrap();
        while !finished.load(Ordering::Acquire) {
            background.status("background", "up_to_date", "").unwrap();
            std::thread::yield_now();
        }
    });
    started.recv().unwrap();
    let result: anyhow::Result<()> = (|| {
        for _ in 0..24 {
            let _guard = a.store.lock()?;
            a.store.save_root(&a.root)?;
            a.store.save_edge(&Edge {
                id: "edge".into(),
                root: a.root.id.clone(),
                host: "test-peer".into(),
                remote_path: "/remote".into(),
            })?;
            engine::observe(&a.store, &a.root)?;
            a.store.unlink(&a.root)?;
        }
        Ok(())
    })();
    stop.store(true, Ordering::Release);
    worker.join().unwrap();
    result.unwrap();
    assert!(a.store.roots().unwrap().is_empty());
    assert!(a.store.edges().unwrap().is_empty());
    assert_eq!(a.read("x"), "recoverable");
    assert!(a.events().iter().all(|c| {
        a.store
            .object(c.entry.kind.hash().unwrap())
            .unwrap()
            .exists()
    }));
}

#[cfg(windows)]
#[test]
fn windows_file_and_directory_symlinks_roundtrip() {
    let a = Device::new();
    let b = Device::new();
    a.write("target/file", "first");
    let root = std::path::Path::new(&a.root.path);
    std::os::windows::fs::symlink_file(r"target\file", root.join("file-link")).unwrap();
    std::os::windows::fs::symlink_dir("target", root.join("directory-link")).unwrap();
    a.observe();
    b.merge(&a);
    assert_eq!(b.read("file-link"), "first");
    assert_eq!(b.read("directory-link/file"), "first");
    b.observe();
    assert_eq!(a.events().len(), b.events().len());
    a.write("target/file", "second");
    a.observe();
    b.merge(&a);
    assert_eq!(b.read("file-link"), "second");
    fs::remove_dir_all(root.join("target")).unwrap();
    a.observe();
    b.merge(&a);
    b.observe();
    assert_eq!(a.events().len(), b.events().len());
    assert!(
        fs::symlink_metadata(std::path::Path::new(&b.root.path).join("directory-link"))
            .unwrap()
            .file_type()
            .is_symlink()
    );
    a.write("target/file", "restored target");
    a.observe();
    b.merge(&a);
    assert_eq!(b.read("directory-link/file"), "restored target");
    fs::remove_file(root.join("file-link")).unwrap();
    fs::remove_dir(root.join("directory-link")).unwrap();
    a.observe();
    b.merge(&a);
    assert!(
        !std::path::Path::new(&b.root.path)
            .join("file-link")
            .exists()
    );
    assert!(
        !std::path::Path::new(&b.root.path)
            .join("directory-link")
            .exists()
    );
}
