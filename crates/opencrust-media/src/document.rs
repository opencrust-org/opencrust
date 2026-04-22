use opencrust_common::{Error, Result};
use std::path::Path;

// ---------------------------------------------------------------------------
// Text extraction
// ---------------------------------------------------------------------------

/// Return true if the filename's extension is supported by [`extract_text`].
pub fn is_supported_for_ingest(filename: &str) -> bool {
    let ext = Path::new(filename)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();
    matches!(
        ext.as_str(),
        "txt"
            | "md"
            | "markdown"
            | "csv"
            | "rs"
            | "py"
            | "js"
            | "ts"
            | "go"
            | "java"
            | "toml"
            | "yaml"
            | "yml"
            | "html"
            | "htm"
            | "json"
            | "pdf"
    )
}

/// Extract plain text from a file based on its extension.
///
/// Supported formats:
/// - `.txt`, `.md`, `.markdown`, `.csv` - read as-is
/// - `.rs`, `.py`, `.js`, `.ts`, `.go`, `.java`, `.toml`, `.yaml`, `.yml` - read as-is (code)
/// - `.html`, `.htm` - strip HTML tags
/// - `.json` - pretty-print JSON
/// - `.pdf` - extract text from PDF pages
pub fn extract_text(path: &Path) -> Result<String> {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();

    // PDF is binary - handle before reading as text
    if ext == "pdf" {
        return extract_pdf_text(path);
    }

    let raw = std::fs::read_to_string(path)
        .map_err(|e| Error::Media(format!("failed to read file {}: {}", path.display(), e)))?;

    match ext.as_str() {
        // Plain text / markdown / CSV / code - return as-is
        "txt" | "md" | "markdown" | "csv" | "rs" | "py" | "js" | "ts" | "go" | "java" | "toml"
        | "yaml" | "yml" => Ok(raw),

        // HTML - strip tags
        "html" | "htm" => Ok(strip_html_tags(&raw)),

        // JSON - pretty-print
        "json" => {
            let value: serde_json::Value = serde_json::from_str(&raw).map_err(|e| {
                Error::Media(format!(
                    "failed to parse JSON from {}: {}",
                    path.display(),
                    e
                ))
            })?;
            serde_json::to_string_pretty(&value)
                .map_err(|e| Error::Media(format!("failed to pretty-print JSON: {}", e)))
        }

        _ => Err(Error::Media(format!(
            "unsupported file extension for text extraction: .{}",
            ext
        ))),
    }
}

/// Extract text from a PDF file.
fn extract_pdf_text(path: &Path) -> Result<String> {
    let bytes = std::fs::read(path)
        .map_err(|e| Error::Media(format!("failed to read PDF {}: {}", path.display(), e)))?;

    let text = pdf_extract::extract_text_from_mem(&bytes).map_err(|e| {
        Error::Media(format!(
            "failed to extract text from PDF {}: {}",
            path.display(),
            e
        ))
    })?;

    let trimmed = text.trim().to_string();
    if trimmed.is_empty() {
        return Err(Error::Media(format!(
            "PDF {} has no extractable text (may be a scanned/image-only PDF)",
            path.display()
        )));
    }

    Ok(trimmed)
}

/// Strip HTML tags from a string.
///
/// Removes `<script>` and `<style>` blocks entirely (including contents),
/// then strips all remaining tags, collapses whitespace, and trims.
fn strip_html_tags(html: &str) -> String {
    // Phase 1: Remove <script>...</script> and <style>...</style> blocks (case-insensitive)
    let without_scripts = remove_blocks(html, "script");
    let without_styles = remove_blocks(&without_scripts, "style");

    // Phase 2: Strip all remaining HTML tags
    let mut result = String::with_capacity(without_styles.len());
    let mut inside_tag = false;

    for ch in without_styles.chars() {
        match ch {
            '<' => inside_tag = true,
            '>' => inside_tag = false,
            _ if !inside_tag => result.push(ch),
            _ => {}
        }
    }

    // Phase 3: Decode common HTML entities
    let result = result
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&apos;", "'")
        .replace("&nbsp;", " ");

    // Phase 4: Collapse runs of whitespace into single spaces, trim
    collapse_whitespace(&result)
}

/// Remove all occurrences of `<tag>...</tag>` (case-insensitive) including contents.
fn remove_blocks(input: &str, tag: &str) -> String {
    let lower = input.to_lowercase();
    let open_tag = format!("<{}", tag);
    let close_tag = format!("</{}>", tag);

    let mut result = String::with_capacity(input.len());
    let mut cursor = 0;

    while cursor < input.len() {
        if let Some(start) = lower[cursor..].find(&open_tag) {
            let abs_start = cursor + start;
            // Push everything before this block
            result.push_str(&input[cursor..abs_start]);

            // Find the closing tag after the opening tag
            if let Some(end) = lower[abs_start..].find(&close_tag) {
                cursor = abs_start + end + close_tag.len();
            } else {
                // No closing tag found - remove the rest
                break;
            }
        } else {
            // No more blocks - push the remainder
            result.push_str(&input[cursor..]);
            break;
        }
    }

    result
}

/// Collapse runs of whitespace (spaces, tabs, newlines) into single spaces and trim.
fn collapse_whitespace(input: &str) -> String {
    let mut result = String::with_capacity(input.len());
    let mut prev_was_space = true; // Start true to trim leading whitespace

    for ch in input.chars() {
        if ch.is_whitespace() {
            if !prev_was_space {
                result.push(' ');
                prev_was_space = true;
            }
        } else {
            result.push(ch);
            prev_was_space = false;
        }
    }

    // Trim trailing space
    if result.ends_with(' ') {
        result.pop();
    }

    result
}

// ---------------------------------------------------------------------------
// MIME type detection
// ---------------------------------------------------------------------------

/// Detect a MIME type from a file path's extension.
///
/// Returns `"application/octet-stream"` for unknown extensions.
pub fn detect_mime_type(path: &Path) -> &'static str {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();

    match ext.as_str() {
        // Images
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "svg" => "image/svg+xml",
        "ico" => "image/x-icon",
        "bmp" => "image/bmp",
        "tiff" | "tif" => "image/tiff",

        // Audio
        "mp3" => "audio/mpeg",
        "ogg" | "oga" => "audio/ogg",
        "wav" => "audio/wav",
        "flac" => "audio/flac",
        "aac" => "audio/aac",
        "m4a" => "audio/mp4",
        "weba" => "audio/webm",

        // Video
        "mp4" | "m4v" => "video/mp4",
        "webm" => "video/webm",
        "avi" => "video/x-msvideo",
        "mov" => "video/quicktime",
        "mkv" => "video/x-matroska",

        // Documents
        "pdf" => "application/pdf",
        "txt" => "text/plain",
        "md" | "markdown" => "text/markdown",
        "html" | "htm" => "text/html",
        "css" => "text/css",
        "csv" => "text/csv",
        "json" => "application/json",
        "xml" => "application/xml",
        "yaml" | "yml" => "application/yaml",
        "toml" => "application/toml",

        // Code
        "rs" => "text/x-rust",
        "py" => "text/x-python",
        "js" => "text/javascript",
        "ts" => "text/typescript",
        "go" => "text/x-go",
        "java" => "text/x-java",

        // Archives
        "zip" => "application/zip",
        "gz" | "gzip" => "application/gzip",
        "tar" => "application/x-tar",

        _ => "application/octet-stream",
    }
}

// ---------------------------------------------------------------------------
// Text chunking
// ---------------------------------------------------------------------------

/// Configuration for text chunking.
pub struct ChunkOptions {
    /// Target chunk size in approximate tokens (default 500).
    pub max_tokens: usize,
    /// Overlap between consecutive chunks in approximate tokens (default 50).
    pub overlap_tokens: usize,
}

impl Default for ChunkOptions {
    fn default() -> Self {
        Self {
            max_tokens: 500,
            overlap_tokens: 50,
        }
    }
}

/// A single chunk of text produced by [`chunk_text`].
#[derive(Debug, Clone)]
pub struct TextChunk {
    /// Zero-based index of this chunk.
    pub index: usize,
    /// The chunk text.
    pub text: String,
    /// Approximate token count (whitespace-based heuristic).
    pub token_count: usize,
}

/// Approximate token count using a word/0.75 heuristic.
fn estimate_tokens(text: &str) -> usize {
    let words = text.split_whitespace().count();
    ((words as f64) / 0.75).ceil() as usize
}

/// Split text into overlapping chunks.
///
/// The algorithm:
/// 1. Split text into paragraphs (double newline).
/// 2. Accumulate paragraphs until reaching `max_tokens`.
/// 3. When a paragraph would exceed `max_tokens`, finalize the current chunk.
/// 4. Start the next chunk with `overlap_tokens` worth of text from the previous chunk's end.
/// 5. If a single paragraph exceeds `max_tokens`, split it on sentence boundaries.
pub fn chunk_text(text: &str, options: &ChunkOptions) -> Vec<TextChunk> {
    if text.is_empty() {
        return vec![];
    }

    let paragraphs = split_paragraphs(text);
    let mut chunks: Vec<TextChunk> = Vec::new();
    let mut current_parts: Vec<String> = Vec::new();
    let mut current_tokens: usize = 0;

    for para in &paragraphs {
        let para_tokens = estimate_tokens(para);

        // If a single paragraph exceeds max_tokens, split it into sentences
        if para_tokens > options.max_tokens {
            // First, flush anything accumulated so far
            if !current_parts.is_empty() {
                finalize_chunk(&mut chunks, &current_parts, current_tokens);
                let overlap = compute_overlap(&current_parts, options.overlap_tokens);
                current_parts = overlap.0;
                current_tokens = overlap.1;
            }

            // Split the large paragraph on sentences and process each
            let sentences = split_sentences(para);
            for sentence in &sentences {
                let sent_tokens = estimate_tokens(sentence);

                if current_tokens + sent_tokens > options.max_tokens && !current_parts.is_empty() {
                    finalize_chunk(&mut chunks, &current_parts, current_tokens);
                    let overlap = compute_overlap(&current_parts, options.overlap_tokens);
                    current_parts = overlap.0;
                    current_tokens = overlap.1;
                }

                current_parts.push(sentence.clone());
                current_tokens += sent_tokens;
            }
        } else if current_tokens + para_tokens > options.max_tokens && !current_parts.is_empty() {
            // Adding this paragraph would exceed the limit - finalize current chunk
            finalize_chunk(&mut chunks, &current_parts, current_tokens);
            let overlap = compute_overlap(&current_parts, options.overlap_tokens);
            current_parts = overlap.0;
            current_tokens = overlap.1;

            current_parts.push(para.clone());
            current_tokens += para_tokens;
        } else {
            current_parts.push(para.clone());
            current_tokens += para_tokens;
        }
    }

    // Flush the last chunk
    if !current_parts.is_empty() {
        finalize_chunk(&mut chunks, &current_parts, current_tokens);
    }

    chunks
}

/// Split text on double-newline boundaries, filtering out empty paragraphs.
fn split_paragraphs(text: &str) -> Vec<String> {
    text.split("\n\n")
        .map(|p| p.trim().to_string())
        .filter(|p| !p.is_empty())
        .collect()
}

/// Split a paragraph into sentences. A sentence boundary is a period, exclamation
/// mark, or question mark followed by whitespace or end-of-string.
fn split_sentences(text: &str) -> Vec<String> {
    let mut sentences = Vec::new();
    let mut current = String::new();
    let chars: Vec<char> = text.chars().collect();

    for i in 0..chars.len() {
        current.push(chars[i]);

        let is_terminal = matches!(chars[i], '.' | '!' | '?');
        let followed_by_space_or_end = i + 1 >= chars.len() || chars[i + 1].is_whitespace();

        if is_terminal && followed_by_space_or_end {
            let trimmed = current.trim().to_string();
            if !trimmed.is_empty() {
                sentences.push(trimmed);
            }
            current.clear();
        }
    }

    // Leftover text that didn't end with a terminal punctuation
    let trimmed = current.trim().to_string();
    if !trimmed.is_empty() {
        sentences.push(trimmed);
    }

    sentences
}

/// Push a finalized chunk onto the chunks vector.
fn finalize_chunk(chunks: &mut Vec<TextChunk>, parts: &[String], token_count: usize) {
    let text = parts.join("\n\n");
    let actual_tokens = estimate_tokens(&text);
    chunks.push(TextChunk {
        index: chunks.len(),
        text,
        token_count: actual_tokens.max(token_count.min(actual_tokens + 1)),
    });
}

/// Compute the overlap portion from the end of the current parts.
///
/// Returns the overlap parts and their token count.
fn compute_overlap(parts: &[String], overlap_tokens: usize) -> (Vec<String>, usize) {
    if overlap_tokens == 0 {
        return (Vec::new(), 0);
    }

    let mut overlap_parts: Vec<String> = Vec::new();
    let mut tokens = 0;

    // Walk backwards through parts to collect overlap
    for part in parts.iter().rev() {
        let part_tokens = estimate_tokens(part);
        if tokens + part_tokens > overlap_tokens && !overlap_parts.is_empty() {
            break;
        }
        overlap_parts.push(part.clone());
        tokens += part_tokens;
    }

    overlap_parts.reverse();
    (overlap_parts, tokens)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::io::Write;

    fn temp_file(name: &str, content: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join("opencrust_doc_test");
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join(name);
        let mut f = fs::File::create(&path).unwrap();
        f.write_all(content.as_bytes()).unwrap();
        path
    }

    // -- extract_text tests --

    #[test]
    fn test_extract_txt() {
        let p = temp_file("hello.txt", "Hello, world!");
        let text = extract_text(&p).unwrap();
        assert_eq!(text, "Hello, world!");
    }

    #[test]
    fn test_extract_markdown() {
        let p = temp_file("readme.md", "# Title\n\nSome *bold* text.");
        let text = extract_text(&p).unwrap();
        assert!(text.contains("# Title"));
    }

    #[test]
    fn test_extract_html() {
        let p = temp_file(
            "page.html",
            "<html><head><style>body{color:red}</style></head><body><p>Hello &amp; world</p></body></html>",
        );
        let text = extract_text(&p).unwrap();
        assert!(text.contains("Hello & world"));
        assert!(!text.contains("<p>"));
        assert!(!text.contains("color:red"));
    }

    #[test]
    fn test_extract_html_with_script() {
        let html = "<div>Before</div><script>alert('xss')</script><div>After</div>";
        let p = temp_file("scripted.html", html);
        let text = extract_text(&p).unwrap();
        assert!(text.contains("Before"));
        assert!(text.contains("After"));
        assert!(!text.contains("alert"));
    }

    #[test]
    fn test_extract_json() {
        let p = temp_file("data.json", r#"{"key":"value","num":42}"#);
        let text = extract_text(&p).unwrap();
        assert!(text.contains("\"key\": \"value\""));
        assert!(text.contains("\"num\": 42"));
    }

    #[test]
    fn test_extract_code_rs() {
        let p = temp_file("main.rs", "fn main() {\n    println!(\"hi\");\n}");
        let text = extract_text(&p).unwrap();
        assert!(text.contains("fn main()"));
    }

    #[test]
    fn test_extract_pdf_invalid() {
        let p = temp_file("bad.pdf", "not a real pdf");
        let result = extract_text(&p);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("PDF") || err.contains("pdf"));
    }

    #[test]
    fn test_extract_unsupported() {
        let p = temp_file("archive.zip", "not really a zip");
        let result = extract_text(&p);
        assert!(result.is_err());
    }

    // -- strip_html_tags tests --

    #[test]
    fn test_strip_html_basic() {
        assert_eq!(strip_html_tags("<b>bold</b>"), "bold");
    }

    #[test]
    fn test_strip_html_entities() {
        assert_eq!(strip_html_tags("a &amp; b &lt; c &gt; d"), "a & b < c > d");
    }

    #[test]
    fn test_strip_html_nested_tags() {
        let html = "<div><p>Hello <strong>world</strong></p></div>";
        assert_eq!(strip_html_tags(html), "Hello world");
    }

    // -- detect_mime_type tests --

    #[test]
    fn test_mime_known_types() {
        assert_eq!(detect_mime_type(Path::new("photo.png")), "image/png");
        assert_eq!(detect_mime_type(Path::new("song.mp3")), "audio/mpeg");
        assert_eq!(detect_mime_type(Path::new("video.mp4")), "video/mp4");
        assert_eq!(detect_mime_type(Path::new("doc.pdf")), "application/pdf");
        assert_eq!(detect_mime_type(Path::new("data.json")), "application/json");
        assert_eq!(detect_mime_type(Path::new("style.css")), "text/css");
        assert_eq!(detect_mime_type(Path::new("code.rs")), "text/x-rust");
        assert_eq!(detect_mime_type(Path::new("app.py")), "text/x-python");
    }

    #[test]
    fn test_mime_unknown() {
        assert_eq!(
            detect_mime_type(Path::new("file.xyz123")),
            "application/octet-stream"
        );
    }

    #[test]
    fn test_mime_no_extension() {
        assert_eq!(
            detect_mime_type(Path::new("Makefile")),
            "application/octet-stream"
        );
    }

    #[test]
    fn test_mime_case_insensitive() {
        assert_eq!(detect_mime_type(Path::new("PHOTO.PNG")), "image/png");
        assert_eq!(detect_mime_type(Path::new("page.HTML")), "text/html");
    }

    // -- estimate_tokens tests --

    #[test]
    fn test_estimate_tokens_empty() {
        assert_eq!(estimate_tokens(""), 0);
    }

    #[test]
    fn test_estimate_tokens_words() {
        // 4 words -> 4 / 0.75 = 5.33 -> ceil = 6
        assert_eq!(estimate_tokens("one two three four"), 6);
    }

    // -- chunk_text tests --

    #[test]
    fn test_chunk_empty() {
        let chunks = chunk_text("", &ChunkOptions::default());
        assert!(chunks.is_empty());
    }

    #[test]
    fn test_chunk_small_text() {
        let text = "A short paragraph.";
        let chunks = chunk_text(text, &ChunkOptions::default());
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].index, 0);
        assert!(chunks[0].text.contains("short paragraph"));
    }

    #[test]
    fn test_chunk_multiple_paragraphs() {
        // Build text with many paragraphs that exceed a small max_tokens
        let paragraphs: Vec<String> = (0..20)
            .map(|i| {
                format!(
                    "This is paragraph number {} with some filler words to add bulk.",
                    i
                )
            })
            .collect();
        let text = paragraphs.join("\n\n");

        let options = ChunkOptions {
            max_tokens: 30,
            overlap_tokens: 5,
        };
        let chunks = chunk_text(&text, &options);

        assert!(chunks.len() > 1, "should produce multiple chunks");

        // All chunks should have sequential indices
        for (i, chunk) in chunks.iter().enumerate() {
            assert_eq!(chunk.index, i);
            assert!(!chunk.text.is_empty());
        }
    }

    #[test]
    fn test_chunk_overlap() {
        let paragraphs: Vec<String> = (0..10)
            .map(|i| format!("Paragraph {} has several words in it for testing.", i))
            .collect();
        let text = paragraphs.join("\n\n");

        let options = ChunkOptions {
            max_tokens: 20,
            overlap_tokens: 5,
        };
        let chunks = chunk_text(&text, &options);

        // With overlap, consecutive chunks should share some text
        if chunks.len() >= 2 {
            // The end of chunk N should appear at the start of chunk N+1
            let last_words_of_first: Vec<&str> =
                chunks[0].text.split_whitespace().rev().take(3).collect();
            let second_text = &chunks[1].text;

            // At least one of the last few words should appear in the next chunk
            let has_overlap = last_words_of_first.iter().any(|w| second_text.contains(w));
            assert!(
                has_overlap,
                "consecutive chunks should have overlapping content"
            );
        }
    }

    #[test]
    fn test_chunk_large_paragraph_sentence_split() {
        // One huge paragraph with clear sentence boundaries
        let sentences: Vec<String> = (0..50)
            .map(|i| format!("Sentence number {} is here.", i))
            .collect();
        let text = sentences.join(" ");

        let options = ChunkOptions {
            max_tokens: 30,
            overlap_tokens: 5,
        };
        let chunks = chunk_text(&text, &options);

        assert!(
            chunks.len() > 1,
            "large paragraph should be split into multiple chunks"
        );

        // Verify no chunk drastically exceeds max_tokens (allow some slack due to overlap)
        for chunk in &chunks {
            // Generous upper bound: max_tokens * 2 accounts for overlap + single large sentence
            assert!(
                chunk.token_count <= options.max_tokens * 2,
                "chunk {} has {} tokens, expected <= {}",
                chunk.index,
                chunk.token_count,
                options.max_tokens * 2
            );
        }
    }

    #[test]
    fn test_split_sentences() {
        let text = "First sentence. Second sentence! Third? And more text";
        let sentences = split_sentences(text);
        assert_eq!(sentences.len(), 4);
        assert_eq!(sentences[0], "First sentence.");
        assert_eq!(sentences[1], "Second sentence!");
        assert_eq!(sentences[2], "Third?");
        assert_eq!(sentences[3], "And more text");
    }

    #[test]
    fn test_split_paragraphs() {
        let text = "Para one.\n\nPara two.\n\n\nPara three.";
        let paras = split_paragraphs(text);
        assert_eq!(paras.len(), 3);
        assert_eq!(paras[0], "Para one.");
        assert_eq!(paras[1], "Para two.");
        assert_eq!(paras[2], "Para three.");
    }
}
