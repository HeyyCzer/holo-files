use super::*;
use holo_core::ProviderKind;

fn store_with_root() -> StateStore {
    let s = StateStore::open_in_memory().unwrap();
    s.upsert_account(&AccountRecord {
        id: "acc".into(),
        provider: ProviderKind::OneDrive,
        remote_id: "r".into(),
        display_name: "Me".into(),
        email: None,
        created_at: Utc::now(),
    })
    .unwrap();
    s.insert_root(&RootRecord {
        id: "root".into(),
        account_id: "acc".into(),
        local_path: r"C:\Sync".into(),
        remote_root_id: RemoteItemId::new("R0"),
        remote_path: "/".into(),
        enabled: true,
        cursor: None,
        created_at: Utc::now(),
    })
    .unwrap();
    s
}

fn item(id: &str, parent: &str, path: &str, dir: bool) -> ItemRecord {
    ItemRecord {
        root_id: "root".into(),
        remote_id: RemoteItemId::new(id),
        parent_id: Some(RemoteItemId::new(parent)),
        name: RelPath::parse(path).unwrap().name().into(),
        rel_path: RelPath::parse(path).unwrap(),
        is_dir: dir,
        size: 3,
        modified: DateTime::from_timestamp_millis(1_700_000_000_000).unwrap(),
        etag: Some("e".into()),
        ctag: None,
        hash: Some(ContentHash {
            algo: holo_core::HashAlgo::QuickXor,
            value: "abc=".into(),
        }),
        synced_at: from_ms(ms(Utc::now())),
    }
}

#[test]
fn migrations_reach_latest_and_are_idempotent() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("s.db");
    assert_eq!(
        StateStore::open(&p).unwrap().schema_version().unwrap(),
        migrations::LATEST
    );
    assert_eq!(
        StateStore::open(&p).unwrap().schema_version().unwrap(),
        migrations::LATEST
    );
}

#[test]
fn item_crud_and_case_insensitive_lookup() {
    let s = store_with_root();
    let i = item("1", "R0", "Docs/A.txt", false);
    s.upsert_item(&i).unwrap();
    assert_eq!(s.item("root", &RemoteItemId::new("1")).unwrap().unwrap(), i);
    assert!(
        s.item_by_path("root", &RelPath::parse("docs/a.TXT").unwrap())
            .unwrap()
            .is_some()
    );
    s.delete_item("root", &i.remote_id).unwrap();
    assert!(s.item("root", &i.remote_id).unwrap().is_none());
}

#[test]
fn subtree_rebase_and_delete() {
    let s = store_with_root();
    s.upsert_item(&item("d", "R0", "a", true)).unwrap();
    s.upsert_item(&item("f", "d", "a/x.txt", false)).unwrap();
    s.upsert_item(&item("g", "R0", "a_b/y.txt", false)).unwrap(); // must not match "a/%"
    assert_eq!(
        s.subtree("root", &RelPath::parse("a").unwrap())
            .unwrap()
            .len(),
        2
    );
    assert_eq!(
        s.rebase_subtree(
            "root",
            &RelPath::parse("a").unwrap(),
            &RelPath::parse("z").unwrap()
        )
        .unwrap(),
        1
    );
    assert!(
        s.item_by_path("root", &RelPath::parse("z/x.txt").unwrap())
            .unwrap()
            .is_some()
    );
    assert_eq!(
        s.delete_subtree("root", &RelPath::parse("z").unwrap())
            .unwrap(),
        1
    );
    assert_eq!(s.item_count("root").unwrap(), 2);
}

#[test]
fn op_journal_lifecycle() {
    let s = store_with_root();
    let a = s.enqueue_op("root", "upload", "{}").unwrap();
    let b = s.enqueue_op("root", "delete", "{}").unwrap();
    assert_eq!(s.next_op("root").unwrap().unwrap().id, a);
    s.retry_op(a, "boom", Utc::now() + chrono::Duration::hours(1))
        .unwrap();
    assert_eq!(s.next_op("root").unwrap().unwrap().id, b);
    s.set_op_state(b, OpState::Held, Some("mass delete"))
        .unwrap();
    assert!(s.next_op("root").unwrap().is_none());
    let c = s.op_counts("root").unwrap();
    assert_eq!((c.pending, c.held), (1, 1));
    assert_eq!(s.release_held_ops(Some("root")).unwrap(), 1);
    assert_eq!(s.next_op("root").unwrap().unwrap().id, b);
    s.complete_op(b).unwrap();
    assert_eq!(s.ops(None).unwrap().len(), 1);
}

#[test]
fn deleting_root_cascades() {
    let s = store_with_root();
    s.upsert_item(&item("1", "R0", "a", false)).unwrap();
    s.enqueue_op("root", "upload", "{}").unwrap();
    s.delete_root("root").unwrap();
    assert_eq!(s.item_count("root").unwrap(), 0);
    assert!(s.ops(None).unwrap().is_empty());
}

#[test]
fn activity_merges_downloads_and_is_bounded() {
    let s = store_with_root();
    let dl = |path, bytes| NewActivity {
        kind: ActivityKind::Downloaded,
        path,
        detail: Some("app.exe"),
        bytes: Some(bytes),
        error: None,
    };
    s.add_activity("root", &dl("a.bin", 10)).unwrap();
    s.add_activity("root", &dl("a.bin", 5)).unwrap();
    let r = s.recent_activity(Some("root"), 10).unwrap();
    assert_eq!(r.len(), 1);
    assert_eq!(r[0].bytes, Some(15));

    let up = NewActivity {
        kind: ActivityKind::Uploaded,
        path: "b.txt",
        detail: None,
        bytes: None,
        error: None,
    };
    for _ in 0..(ACTIVITY_KEEP + 5) {
        s.add_activity("root", &up).unwrap();
    }
    let r = s.recent_activity(None, 5000).unwrap();
    assert_eq!(r.len() as i64, ACTIVITY_KEEP);
    assert_eq!(r[0].kind, Some(ActivityKind::Uploaded));
}
