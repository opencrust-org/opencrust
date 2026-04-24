use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use opencrust_common::Result;
use opencrust_db::{DocumentStore, NewDocumentChunk};
use opencrust_gateway::ingest::{IngestResult, ingest_from_path};
use opencrust_media::{ChunkOptions, chunk_text};

const FIXTURE_NAME: &str = "README.md";
const FIXTURE_TEXT: &str = include_str!("../../../README.md");
const ITERATIONS: usize = 3;

#[derive(Debug)]
struct TimedRun {
    duration: Duration,
    chunk_count: usize,
}

fn median_duration(durations: &[Duration]) -> Duration {
    let mut durations = durations.to_vec();
    durations.sort_unstable_by_key(|d| d.as_nanos());
    durations[durations.len() / 2]
}

fn mean_duration(durations: &[Duration]) -> Duration {
    let total_nanos: u128 = durations.iter().map(Duration::as_nanos).sum();
    let mean_nanos = total_nanos / durations.len() as u128;
    Duration::from_nanos(mean_nanos as u64)
}

fn format_duration(duration: Duration) -> String {
    format!("{:.2} ms", duration.as_secs_f64() * 1000.0)
}

fn fixture_file() -> Result<(tempfile::TempDir, PathBuf)> {
    let dir = tempfile::tempdir().map_err(|e| {
        opencrust_common::Error::Agent(format!(
            "failed to create tempdir for benchmark fixture: {e}"
        ))
    })?;
    let path = dir.path().join(FIXTURE_NAME);
    fs::write(&path, FIXTURE_TEXT).map_err(|e| {
        opencrust_common::Error::Agent(format!("failed to write benchmark fixture: {e}"))
    })?;
    Ok((dir, path))
}

fn doc_store_in_tempdir(dir: &tempfile::TempDir) -> Result<DocumentStore> {
    DocumentStore::open(&dir.path().join("benchmark.db"))
}

fn legacy_ingest_from_path(path: &Path, doc_store: &DocumentStore) -> Result<IngestResult> {
    let text = opencrust_media::extract_text(path)?;
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| path.display().to_string());
    let source_path = path.display().to_string();
    let mime = opencrust_media::detect_mime_type(path);
    let chunks = chunk_text(&text, &ChunkOptions::default());

    let doc_id = doc_store.add_document(&name, Some(source_path.as_str()), mime)?;
    for chunk in &chunks {
        doc_store.add_chunk(
            &doc_id,
            chunk.index,
            &chunk.text,
            None,
            None,
            None,
            Some(chunk.token_count),
        )?;
    }
    doc_store.update_chunk_count(&doc_id, chunks.len())?;

    let doc = doc_store
        .get_document_by_name(&name)?
        .expect("document should exist after legacy ingest");
    assert_eq!(doc.chunk_count, chunks.len());

    Ok(IngestResult {
        name,
        chunk_count: chunks.len(),
        has_embeddings: false,
        replaced: false,
    })
}

fn legacy_store_chunks(
    doc_store: &DocumentStore,
    name: &str,
    source_path: Option<String>,
    mime: &str,
    chunks: &[opencrust_media::TextChunk],
) -> Result<IngestResult> {
    let doc_id = doc_store.add_document(name, source_path.as_deref(), mime)?;
    for chunk in chunks {
        doc_store.add_chunk(
            &doc_id,
            chunk.index,
            &chunk.text,
            None,
            None,
            None,
            Some(chunk.token_count),
        )?;
    }
    doc_store.update_chunk_count(&doc_id, chunks.len())?;

    let doc = doc_store
        .get_document_by_name(name)?
        .expect("document should exist after legacy store");
    assert_eq!(doc.chunk_count, chunks.len());

    Ok(IngestResult {
        name: name.to_string(),
        chunk_count: chunks.len(),
        has_embeddings: false,
        replaced: false,
    })
}

async fn batch_ingest_from_path(path: &Path, doc_store: &DocumentStore) -> Result<IngestResult> {
    ingest_from_path(path, doc_store, None, false).await
}

fn batch_store_chunks(
    doc_store: &DocumentStore,
    name: &str,
    source_path: Option<String>,
    mime: &str,
    chunks: &[opencrust_media::TextChunk],
) -> Result<IngestResult> {
    let doc_id = doc_store.add_document(name, source_path.as_deref(), mime)?;
    let batch_chunks = chunks
        .iter()
        .map(|chunk| NewDocumentChunk {
            chunk_index: chunk.index,
            text: &chunk.text,
            embedding: None,
            model: None,
            dims: None,
            token_count: Some(chunk.token_count),
        })
        .collect::<Vec<_>>();

    doc_store.add_chunks_batch(&doc_id, &batch_chunks)?;

    let doc = doc_store
        .get_document_by_name(name)?
        .expect("document should exist after batch store");
    assert_eq!(doc.chunk_count, chunks.len());

    Ok(IngestResult {
        name: name.to_string(),
        chunk_count: chunks.len(),
        has_embeddings: false,
        replaced: false,
    })
}

#[tokio::test]
#[ignore = "benchmark"]
async fn benchmark_full_ingest_path() {
    let mut legacy_times = Vec::with_capacity(ITERATIONS);
    let mut batch_times = Vec::with_capacity(ITERATIONS);

    for run in 0..ITERATIONS {
        let legacy_first = run % 2 == 0;

        let (legacy, batch) = if legacy_first {
            let (legacy_dir, legacy_path) = fixture_file().expect("fixture file");
            let legacy_store = doc_store_in_tempdir(&legacy_dir).expect("legacy store");
            let legacy_started = Instant::now();
            let legacy =
                legacy_ingest_from_path(&legacy_path, &legacy_store).expect("legacy full ingest");
            let legacy_elapsed = legacy_started.elapsed();

            let (batch_dir, batch_path) = fixture_file().expect("fixture file");
            let batch_store = doc_store_in_tempdir(&batch_dir).expect("batch store");
            let batch_started = Instant::now();
            let batch = batch_ingest_from_path(&batch_path, &batch_store)
                .await
                .expect("batch full ingest");
            let batch_elapsed = batch_started.elapsed();

            (
                TimedRun {
                    duration: legacy_elapsed,
                    chunk_count: legacy.chunk_count,
                },
                TimedRun {
                    duration: batch_elapsed,
                    chunk_count: batch.chunk_count,
                },
            )
        } else {
            let (batch_dir, batch_path) = fixture_file().expect("fixture file");
            let batch_store = doc_store_in_tempdir(&batch_dir).expect("batch store");
            let batch_started = Instant::now();
            let batch = batch_ingest_from_path(&batch_path, &batch_store)
                .await
                .expect("batch full ingest");
            let batch_elapsed = batch_started.elapsed();

            let (legacy_dir, legacy_path) = fixture_file().expect("fixture file");
            let legacy_store = doc_store_in_tempdir(&legacy_dir).expect("legacy store");
            let legacy_started = Instant::now();
            let legacy =
                legacy_ingest_from_path(&legacy_path, &legacy_store).expect("legacy full ingest");
            let legacy_elapsed = legacy_started.elapsed();

            (
                TimedRun {
                    duration: legacy_elapsed,
                    chunk_count: legacy.chunk_count,
                },
                TimedRun {
                    duration: batch_elapsed,
                    chunk_count: batch.chunk_count,
                },
            )
        };

        assert_eq!(legacy.chunk_count, batch.chunk_count);

        println!(
            "full ingest run {}: legacy {}, batch {}, chunks {}",
            run + 1,
            format_duration(legacy.duration),
            format_duration(batch.duration),
            legacy.chunk_count
        );

        legacy_times.push(legacy.duration);
        batch_times.push(batch.duration);
    }

    let legacy_mean = mean_duration(&legacy_times);
    let batch_mean = mean_duration(&batch_times);
    let legacy_median = median_duration(&legacy_times);
    let batch_median = median_duration(&batch_times);
    let speedup = legacy_median.as_secs_f64() / batch_median.as_secs_f64();
    let mean_speedup = legacy_mean.as_secs_f64() / batch_mean.as_secs_f64();

    println!(
        "full ingest summary: legacy median {}, batch median {}, speedup {:.2}x",
        format_duration(legacy_median),
        format_duration(batch_median),
        speedup
    );
    println!(
        "full ingest mean: legacy mean {}, batch mean {}, speedup {:.2}x",
        format_duration(legacy_mean),
        format_duration(batch_mean),
        mean_speedup
    );
}

#[test]
#[ignore = "benchmark"]
fn benchmark_document_store_batch_vs_legacy() {
    let chunks = chunk_text(FIXTURE_TEXT, &ChunkOptions::default());
    let mime = opencrust_media::detect_mime_type(Path::new(FIXTURE_NAME));

    let mut legacy_times = Vec::with_capacity(ITERATIONS);
    let mut batch_times = Vec::with_capacity(ITERATIONS);

    for run in 0..ITERATIONS {
        let legacy_first = run % 2 == 0;

        let (legacy, batch) = if legacy_first {
            let legacy_dir = tempfile::tempdir().expect("legacy tempdir");
            let legacy_store = doc_store_in_tempdir(&legacy_dir).expect("legacy store");
            let legacy_started = Instant::now();
            let legacy = legacy_store_chunks(&legacy_store, FIXTURE_NAME, None, mime, &chunks)
                .expect("legacy store chunks");
            let legacy_elapsed = legacy_started.elapsed();

            let batch_dir = tempfile::tempdir().expect("batch tempdir");
            let batch_store = doc_store_in_tempdir(&batch_dir).expect("batch store");
            let batch_started = Instant::now();
            let batch = batch_store_chunks(&batch_store, FIXTURE_NAME, None, mime, &chunks)
                .expect("batch store chunks");
            let batch_elapsed = batch_started.elapsed();

            (
                TimedRun {
                    duration: legacy_elapsed,
                    chunk_count: legacy.chunk_count,
                },
                TimedRun {
                    duration: batch_elapsed,
                    chunk_count: batch.chunk_count,
                },
            )
        } else {
            let batch_dir = tempfile::tempdir().expect("batch tempdir");
            let batch_store = doc_store_in_tempdir(&batch_dir).expect("batch store");
            let batch_started = Instant::now();
            let batch = batch_store_chunks(&batch_store, FIXTURE_NAME, None, mime, &chunks)
                .expect("batch store chunks");
            let batch_elapsed = batch_started.elapsed();

            let legacy_dir = tempfile::tempdir().expect("legacy tempdir");
            let legacy_store = doc_store_in_tempdir(&legacy_dir).expect("legacy store");
            let legacy_started = Instant::now();
            let legacy = legacy_store_chunks(&legacy_store, FIXTURE_NAME, None, mime, &chunks)
                .expect("legacy store chunks");
            let legacy_elapsed = legacy_started.elapsed();

            (
                TimedRun {
                    duration: legacy_elapsed,
                    chunk_count: legacy.chunk_count,
                },
                TimedRun {
                    duration: batch_elapsed,
                    chunk_count: batch.chunk_count,
                },
            )
        };

        assert_eq!(legacy.chunk_count, batch.chunk_count);

        println!(
            "db write run {}: legacy {}, batch {}, chunks {}",
            run + 1,
            format_duration(legacy.duration),
            format_duration(batch.duration),
            legacy.chunk_count
        );

        legacy_times.push(legacy.duration);
        batch_times.push(batch.duration);
    }

    let legacy_mean = mean_duration(&legacy_times);
    let batch_mean = mean_duration(&batch_times);
    let legacy_median = median_duration(&legacy_times);
    let batch_median = median_duration(&batch_times);
    let speedup = legacy_median.as_secs_f64() / batch_median.as_secs_f64();
    let mean_speedup = legacy_mean.as_secs_f64() / batch_mean.as_secs_f64();

    println!(
        "db write summary: legacy median {}, batch median {}, speedup {:.2}x",
        format_duration(legacy_median),
        format_duration(batch_median),
        speedup
    );
    println!(
        "db write mean: legacy mean {}, batch mean {}, speedup {:.2}x",
        format_duration(legacy_mean),
        format_duration(batch_mean),
        mean_speedup
    );
}
