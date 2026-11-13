use std::sync::Arc;

use cs_postgres::PgStore;
use cs_storage::{CognitiveStore, InMemoryStore};

#[tokio::test]
async fn in_memory_store_conforms() {
    cs_conformance::run_all(|| Arc::new(InMemoryStore::new()) as Arc<dyn CognitiveStore>).await;
}

/// Runs when `CS_TEST_DATABASE_URL` points at a PostgreSQL with pgvector.
#[tokio::test]
async fn postgres_store_conforms() {
    let Ok(url) = std::env::var("CS_TEST_DATABASE_URL") else {
        eprintln!("skipping: CS_TEST_DATABASE_URL not set");
        return;
    };
    let store = PgStore::connect(&url, 5).await.expect("connect");
    store.migrate().await.expect("migrate");
    cs_conformance::run_all(|| Arc::new(store.clone()) as Arc<dyn CognitiveStore>).await;
}

/// Append-only enforcement lives in the database, not just in Rust.
#[tokio::test]
async fn postgres_event_log_rejects_mutation() {
    let Ok(url) = std::env::var("CS_TEST_DATABASE_URL") else {
        return;
    };
    let store = PgStore::connect(&url, 2).await.expect("connect");
    store.migrate().await.expect("migrate");
    let h = cs_conformance::Harness::new(Arc::new(store.clone()));
    h.engine
        .create_memory(&h.alice, h.fact(h.alice_scope(), "anything"))
        .await
        .unwrap();
    for sql in [
        "UPDATE cognitive_events SET payload = '{}'::jsonb",
        "DELETE FROM cognitive_events",
        "TRUNCATE cognitive_events",
    ] {
        let err = match sqlx::query(sql).execute(store.pool()).await {
            Ok(_) => "succeeded".to_owned(),
            Err(e) => e.to_string(),
        };
        assert!(err.contains("append-only"), "{sql}: {err}");
    }
}
