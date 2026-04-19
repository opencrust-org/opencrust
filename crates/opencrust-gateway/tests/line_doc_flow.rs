/// Integration tests for the LINE document ingestion flow.
///
/// Simulates the full user journey through the AppState layer:
///   1. File arrives via LINE webhook → stored as pending
///   2. User sends `!ingest` → file ingested into document store
///   3. User asks about the document → doc_search finds the content
use std::sync::Arc;

use opencrust_agents::AgentRuntime;
use opencrust_channels::ChannelRegistry;
use opencrust_config::AppConfig;
use opencrust_db::DocumentStore;
use opencrust_gateway::{
    ingest::run_ingest,
    state::{AppState, PendingFile},
};

fn test_state() -> AppState {
    AppState::new(
        AppConfig::default(),
        Arc::new(AgentRuntime::new()),
        ChannelRegistry::new(),
    )
}

// ── Step 1: File received → stored as pending ─────────────────────────────

#[test]
fn file_received_stored_as_pending() {
    let state = test_state();
    let session_id = "line-Uabc123";

    assert!(!state.has_pending_file(session_id));

    state.set_pending_file(
        session_id,
        PendingFile {
            filename: "report.pdf".to_string(),
            data: b"PDF content about quarterly sales figures.".to_vec(),
            received_at: std::time::Instant::now(),
        },
    );

    assert!(state.has_pending_file(session_id));
}

// ── Step 2: !ingest consumes the pending file ─────────────────────────────

#[tokio::test]
async fn ingest_command_consumes_pending_file_and_stores_document() {
    let state = test_state();
    let session_id = "line-Uabc123";
    let data_dir = tempfile::tempdir().expect("tempdir");

    let content = b"This document explains the quarterly sales report for Q3 2024.";

    state.set_pending_file(
        session_id,
        PendingFile {
            filename: "q3_report.txt".to_string(),
            data: content.to_vec(),
            received_at: std::time::Instant::now(),
        },
    );

    // Simulate `!ingest` command
    let pending = state
        .take_pending_file(session_id)
        .expect("pending file should exist");

    let response = run_ingest(&state, data_dir.path(), "!ingest", &pending.filename, &pending.data)
        .await
        .expect("ingest should succeed");

    let text = response.text();
    assert!(
        text.contains("q3_report.txt"),
        "response should mention filename, got: {text}"
    );
    assert!(
        text.contains("chunk") || text.contains("Ingested"),
        "response should confirm ingestion, got: {text}"
    );

    // File should now be gone from pending
    assert!(
        !state.has_pending_file(session_id),
        "pending file should be consumed after !ingest"
    );
}

// ── Step 3: After ingest, doc_search finds the content ────────────────────

#[tokio::test]
async fn after_ingest_doc_search_finds_content() {
    use opencrust_agents::tools::{DocSearchTool, Tool, ToolContext};

    let data_dir = tempfile::tempdir().expect("tempdir");
    let db_path = data_dir.path().join("memory.db");

    // Ingest a document directly into the store
    let content = "The quarterly sales report shows revenue grew 15% year-over-year in Q3 2024.";
    {
        let store = DocumentStore::open(&db_path).expect("open store");
        let doc_id = store
            .add_document("q3_report.txt", None, "text/plain")
            .expect("add document");
        store
            .add_chunk(&doc_id, 0, content, None, None, None, None)
            .expect("add chunk");
        store.update_chunk_count(&doc_id, 1).expect("update count");
    }

    let tool = DocSearchTool::new(db_path, None);
    let ctx = ToolContext {
        session_id: "line-Uabc123".to_string(),
        user_id: None,
        heartbeat_depth: 0,
        allowed_tools: None,
    };

    // Ask about document content
    let output = tool
        .execute(&ctx, serde_json::json!({"query": "quarterly sales revenue Q3"}))
        .await
        .expect("doc_search should succeed");

    assert!(
        !output.is_error,
        "doc_search returned error: {}",
        output.content
    );
    assert!(
        output.content.contains("q3_report.txt"),
        "result should reference source document, got: {}",
        output.content
    );
    assert!(
        output.content.contains("revenue") || output.content.contains("sales"),
        "result should contain relevant content, got: {}",
        output.content
    );
}

// ── Step 4: !ingest without a pending file returns a helpful message ───────

#[tokio::test]
async fn ingest_without_pending_file_returns_hint() {
    let state = test_state();
    let data_dir = tempfile::tempdir().expect("tempdir");
    let session_id = "line-Uabc999";

    // No file was set for this session
    assert!(!state.has_pending_file(session_id));

    // The on_message handler checks has_pending_file before calling take_pending_file.
    // Simulate that check here: if no pending file, the bot returns a hint message.
    let pending = state.take_pending_file(session_id);
    assert!(
        pending.is_none(),
        "take_pending_file should return None when nothing is pending"
    );

    // Calling run_ingest with whitespace-only data returns Err (no extractable text)
    let result = run_ingest(&state, data_dir.path(), "!ingest", "empty.txt", b"   ").await;
    match result {
        Ok(resp) => {
            let text = resp.text().to_string();
            assert!(
                text.contains("no text") || text.contains("Failed"),
                "ok-path should indicate no content, got: {text}"
            );
        }
        Err(e) => {
            assert!(
                e.contains("no text content") || e.contains("Failed"),
                "err-path should indicate no content, got: {e}"
            );
        }
    }
}

// ── Step 5: Pending file expires after TTL ────────────────────────────────

#[test]
fn pending_file_expires_after_ttl() {
    let state = test_state();
    let session_id = "line-Uexpired";

    // Inject a file that was received >5 minutes ago
    state.set_pending_file(
        session_id,
        PendingFile {
            filename: "old.pdf".to_string(),
            data: vec![1, 2, 3],
            received_at: std::time::Instant::now()
                - std::time::Duration::from_secs(301), // just over 5-min TTL
        },
    );

    // take_pending_file should filter out expired files
    let result = state.take_pending_file(session_id);
    assert!(
        result.is_none(),
        "expired pending file should not be returned"
    );
}

// ── Step 6: !ingest replace overwrites an existing document ───────────────

#[tokio::test]
async fn ingest_replace_overwrites_existing_document() {
    let state = test_state();
    let data_dir = tempfile::tempdir().expect("tempdir");

    let original = b"Original content from first upload.";
    let updated = b"Updated content after revision.";

    // First ingest
    run_ingest(&state, data_dir.path(), "!ingest", "notes.txt", original)
        .await
        .expect("first ingest should succeed");

    // Second ingest without replace → should indicate already ingested
    let resp2 = run_ingest(&state, data_dir.path(), "!ingest", "notes.txt", original)
        .await
        .expect("second ingest should return Ok with hint");
    assert!(
        resp2.text().contains("already ingested"),
        "should mention already ingested: {}",
        resp2.text()
    );

    // Third ingest with replace keyword → should succeed
    let resp3 = run_ingest(
        &state,
        data_dir.path(),
        "!ingest replace",
        "notes.txt",
        updated,
    )
    .await
    .expect("replace ingest should succeed");
    assert!(
        resp3.text().contains("Replaced") || resp3.text().contains("notes.txt"),
        "should confirm replacement: {}",
        resp3.text()
    );
}

// ── Step 7: doc_search returns empty for unknown content ──────────────────

#[tokio::test]
async fn doc_search_returns_empty_when_no_matching_content() {
    use opencrust_agents::tools::{DocSearchTool, Tool, ToolContext};

    let data_dir = tempfile::tempdir().expect("tempdir");
    let db_path = data_dir.path().join("memory.db");

    // Initialize the store with one unrelated document
    {
        let store = DocumentStore::open(&db_path).expect("open store");
        let doc_id = store
            .add_document("cats.txt", None, "text/plain")
            .expect("add document");
        store
            .add_chunk(&doc_id, 0, "Cats are great pets.", None, None, None, None)
            .expect("add chunk");
        store.update_chunk_count(&doc_id, 1).expect("update count");
    }

    let tool = DocSearchTool::new(db_path, None);
    let ctx = ToolContext {
        session_id: "line-Uabc123".to_string(),
        user_id: None,
        heartbeat_depth: 0,
        allowed_tools: None,
    };

    // Query about something completely unrelated
    let output = tool
        .execute(&ctx, serde_json::json!({"query": "blockchain cryptocurrency defi"}))
        .await
        .expect("doc_search should not error");

    assert!(
        !output.is_error,
        "doc_search should not error on empty results"
    );
    assert!(
        output.content.contains("No relevant"),
        "should indicate no results found, got: {}",
        output.content
    );
}
