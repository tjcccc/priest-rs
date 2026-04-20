use tempfile::TempDir;
use priest::session::in_memory::InMemorySessionStore;
use priest::session::sqlite::SqliteSessionStore;
use priest::session::store::SessionStore;

// ── InMemorySessionStore ──────────────────────────────────────────────────────

#[tokio::test]
async fn in_memory_create_and_get() {
    let store = InMemorySessionStore::new();
    let sess = store.create("default", Some("s1")).await.unwrap();
    assert_eq!(sess.id, "s1");
    let found = store.get("s1").await.unwrap();
    assert!(found.is_some());
}

#[tokio::test]
async fn in_memory_get_returns_none_for_missing() {
    let store = InMemorySessionStore::new();
    let found = store.get("ghost").await.unwrap();
    assert!(found.is_none());
}

#[tokio::test]
async fn in_memory_save_persists_turns() {
    let store = InMemorySessionStore::new();
    let mut sess = store.create("default", Some("s2")).await.unwrap();
    sess.append_turn("user", "Hello");
    sess.append_turn("assistant", "Hi");
    store.save(&sess).await.unwrap();

    let loaded = store.get("s2").await.unwrap().unwrap();
    assert_eq!(loaded.turns.len(), 2);
    assert_eq!(loaded.turns[0].content, "Hello");
}

#[tokio::test]
async fn in_memory_create_with_no_id_generates_uuid() {
    let store = InMemorySessionStore::new();
    let sess = store.create("default", None).await.unwrap();
    assert!(!sess.id.is_empty());
    assert_eq!(sess.id.len(), 36); // UUID v4 format
}

// ── SqliteSessionStore ────────────────────────────────────────────────────────

#[tokio::test]
async fn sqlite_create_and_get() {
    let tmp = TempDir::new().unwrap();
    let store = SqliteSessionStore::open(tmp.path().join("test.db")).unwrap();
    let sess = store.create("default", Some("s1")).await.unwrap();
    assert_eq!(sess.id, "s1");

    let found = store.get("s1").await.unwrap().unwrap();
    assert_eq!(found.id, "s1");
    assert_eq!(found.profile_name, "default");
}

#[tokio::test]
async fn sqlite_get_returns_none_for_missing() {
    let tmp = TempDir::new().unwrap();
    let store = SqliteSessionStore::open(tmp.path().join("test.db")).unwrap();
    assert!(store.get("ghost").await.unwrap().is_none());
}

#[tokio::test]
async fn sqlite_save_and_reload_turns() {
    let tmp = TempDir::new().unwrap();
    let store = SqliteSessionStore::open(tmp.path().join("test.db")).unwrap();
    let mut sess = store.create("default", Some("s2")).await.unwrap();
    sess.append_turn("user", "Hi");
    sess.append_turn("assistant", "Hello");
    store.save(&sess).await.unwrap();

    let loaded = store.get("s2").await.unwrap().unwrap();
    assert_eq!(loaded.turns.len(), 2);
    assert_eq!(loaded.turns[0].role, "user");
    assert_eq!(loaded.turns[1].role, "assistant");
}

#[tokio::test]
async fn sqlite_save_replaces_turns_on_second_save() {
    let tmp = TempDir::new().unwrap();
    let store = SqliteSessionStore::open(tmp.path().join("test.db")).unwrap();
    let mut sess = store.create("default", Some("s3")).await.unwrap();
    sess.append_turn("user", "first");
    store.save(&sess).await.unwrap();

    sess.append_turn("assistant", "reply");
    store.save(&sess).await.unwrap();

    let loaded = store.get("s3").await.unwrap().unwrap();
    assert_eq!(loaded.turns.len(), 2);
}

#[tokio::test]
async fn sqlite_persists_across_reopen() {
    let tmp = TempDir::new().unwrap();
    let db_path = tmp.path().join("test.db");

    {
        let store = SqliteSessionStore::open(&db_path).unwrap();
        let mut sess = store.create("default", Some("persistent")).await.unwrap();
        sess.append_turn("user", "remembered");
        store.save(&sess).await.unwrap();
    }

    {
        let store = SqliteSessionStore::open(&db_path).unwrap();
        let sess = store.get("persistent").await.unwrap().unwrap();
        assert_eq!(sess.turns[0].content, "remembered");
    }
}
