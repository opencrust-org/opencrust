use opencrust_common::Result;
use std::path::{Path, PathBuf};

/// Media processing pipeline for images, audio, and video.
///
/// Wraps external tools (ffmpeg, etc.) and provides a consistent API
/// for media operations needed by channels and agents.
pub struct MediaProcessor {
    allowed_paths: Vec<PathBuf>,
}

impl MediaProcessor {
    pub fn new<P: AsRef<Path>>(allowed_paths: impl IntoIterator<Item = P>) -> Self {
        let mut paths = Vec::new();
        for path in allowed_paths {
            if let Ok(canonical) = std::fs::canonicalize(path) {
                paths.push(canonical);
            }
        }
        Self {
            allowed_paths: paths,
        }
    }

    /// Check if ffmpeg is available on the system.
    pub fn ffmpeg_available() -> bool {
        std::process::Command::new("ffmpeg")
            .arg("-version")
            .output()
            .is_ok()
    }

    #[allow(clippy::collapsible_if)]
    fn validate_path(&self, path: &Path, is_output: bool) -> Result<PathBuf> {
        if is_output {
            if let Ok(metadata) = path.symlink_metadata() {
                if metadata.is_symlink() {
                    return Err(opencrust_common::Error::Security(format!(
                        "Output path cannot be a symlink: {}",
                        path.display()
                    )));
                }
            }
        }

        let canonical_path = if is_output && !path.exists() {
            // For output files that don't exist yet, check the parent directory
            let parent = path.parent().unwrap_or_else(|| Path::new("."));
            let canonical_parent = std::fs::canonicalize(parent).map_err(|_| {
                opencrust_common::Error::Security(format!(
                    "Output directory does not exist or is invalid: {}",
                    parent.display()
                ))
            })?;
            // Ensure the filename part doesn't contain separators (already handled by Path logic usually, but good to be safe)
            if let Some(file_name) = path.file_name() {
                if file_name
                    .to_string_lossy()
                    .contains(std::path::MAIN_SEPARATOR)
                {
                    return Err(opencrust_common::Error::Security("Invalid filename".into()));
                }
            }
            // We return the construct of canonical_parent + filename for check?
            // Actually, we just need to ensure canonical_parent is allowed.
            // If canonical_parent is allowed, then writing a file inside it is allowed.
            // But we should return the full path for ffmpeg to use?
            // Ffmpeg takes the path as provided or absolute?
            // It's safer to pass the absolute path to ffmpeg.
            canonical_parent.join(path.file_name().unwrap_or_default())
        } else {
            std::fs::canonicalize(path).map_err(|_| {
                opencrust_common::Error::Security(format!(
                    "Path not found or invalid: {}",
                    path.display()
                ))
            })?
        };

        // Check if the path starts with any of the allowed paths
        if !self
            .allowed_paths
            .iter()
            .any(|allowed| canonical_path.starts_with(allowed))
        {
            return Err(opencrust_common::Error::Security(format!(
                "Path not allowed: {}",
                path.display()
            )));
        }

        Ok(canonical_path)
    }

    /// Convert audio to a target format using ffmpeg.
    pub async fn convert_audio(
        &self,
        input: &std::path::Path,
        output: &std::path::Path,
        _format: &str,
    ) -> Result<()> {
        let input_path = self.validate_path(input, false)?;
        let output_path = self.validate_path(output, true)?;

        let status = tokio::process::Command::new("ffmpeg")
            .args([
                "-i",
                &input_path.to_string_lossy(),
                "-y",
                &output_path.to_string_lossy(),
            ])
            .status()
            .await?;

        if !status.success() {
            return Err(opencrust_common::Error::Media(
                "ffmpeg conversion failed".into(),
            ));
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use opencrust_common::Error;
    use std::fs::File;
    use std::io::Write;

    #[tokio::test]
    async fn test_allowed_paths_validation() -> Result<()> {
        let temp_dir = std::env::temp_dir().join("opencrust_media_test_allowed_2");
        if temp_dir.exists() {
            std::fs::remove_dir_all(&temp_dir).unwrap();
        }
        std::fs::create_dir_all(&temp_dir).unwrap();
        let input_file = temp_dir.join("input.mp3");
        File::create(&input_file)
            .unwrap()
            .write_all(b"dummy")
            .unwrap();

        let output_file = temp_dir.join("output.mp3");

        let processor = MediaProcessor::new(vec![&temp_dir]);

        // Valid case: input and output in allowed dir
        // This will likely fail with IO error (ffmpeg missing) or Media error (dummy file), but SHOULD NOT be Security error
        let result = processor
            .convert_audio(&input_file, &output_file, "mp3")
            .await;
        match result {
            Ok(_) => {} // ffmpeg worked (unlikely)
            Err(Error::Security(_)) => panic!("Valid path rejected as security error"),
            Err(_) => {} // Other errors are expected (ffmpeg missing/failed)
        }

        // Cleanup
        std::fs::remove_dir_all(&temp_dir).unwrap();
        Ok(())
    }

    #[tokio::test]
    async fn test_disallowed_paths_validation() -> Result<()> {
        let temp_dir = std::env::temp_dir().join("opencrust_media_test_denied_2");
        if temp_dir.exists() {
            std::fs::remove_dir_all(&temp_dir).unwrap();
        }
        std::fs::create_dir_all(&temp_dir).unwrap();
        let input_file = temp_dir.join("input.mp3");
        File::create(&input_file)
            .unwrap()
            .write_all(b"dummy")
            .unwrap();

        let other_dir = std::env::temp_dir().join("opencrust_media_test_other_2");
        if other_dir.exists() {
            std::fs::remove_dir_all(&other_dir).unwrap();
        }
        std::fs::create_dir_all(&other_dir).unwrap();
        let output_file = other_dir.join("output.mp3");

        // Allowed only temp_dir, not other_dir
        let processor = MediaProcessor::new(vec![&temp_dir]);

        // Invalid output path (in other_dir)
        let result = processor
            .convert_audio(&input_file, &output_file, "mp3")
            .await;
        match result {
            Err(Error::Security(msg)) => assert!(msg.contains("Path not allowed")),
            _ => panic!("Disallowed output path was not rejected"),
        }

        // Invalid input path (if we try to read from other_dir)
        let other_input = other_dir.join("input_other.mp3");
        File::create(&other_input).unwrap();
        let valid_output = temp_dir.join("output_valid.mp3");

        let result = processor
            .convert_audio(&other_input, &valid_output, "mp3")
            .await;
        match result {
            Err(Error::Security(msg)) => assert!(msg.contains("Path not allowed")),
            _ => panic!("Disallowed input path was not rejected"),
        }

        std::fs::remove_dir_all(&temp_dir).unwrap();
        std::fs::remove_dir_all(&other_dir).unwrap();
        Ok(())
    }

    #[tokio::test]
    async fn test_path_traversal_prevention() -> Result<()> {
        let temp_dir = std::env::temp_dir().join("opencrust_media_test_traversal_2");
        if temp_dir.exists() {
            std::fs::remove_dir_all(&temp_dir).unwrap();
        }
        std::fs::create_dir_all(&temp_dir).unwrap();
        let input_file = temp_dir.join("input.mp3");
        File::create(&input_file).unwrap();

        let processor = MediaProcessor::new(vec![&temp_dir]);

        // Try to output to parent of temp_dir
        let traversal_output = temp_dir.join("..").join("output_evil.mp3");

        let result = processor
            .convert_audio(&input_file, &traversal_output, "mp3")
            .await;
        match result {
            Err(Error::Security(msg)) => assert!(msg.contains("Path not allowed")),
            Ok(_) => panic!("Traversal path was allowed!"),
            Err(e) => panic!("Unexpected error type: {:?}", e),
        }

        std::fs::remove_dir_all(&temp_dir).unwrap();
        Ok(())
    }

    #[tokio::test]
    #[cfg(unix)]
    async fn test_dangling_symlink_rejection() -> Result<()> {
        use std::os::unix::fs::symlink;

        let temp_dir = std::env::temp_dir().join("opencrust_media_test_symlink_allowed_2");
        let forbidden_dir = std::env::temp_dir().join("opencrust_media_test_symlink_forbidden_2");

        if temp_dir.exists() {
            std::fs::remove_dir_all(&temp_dir).unwrap();
        }
        if forbidden_dir.exists() {
            std::fs::remove_dir_all(&forbidden_dir).unwrap();
        }

        std::fs::create_dir_all(&temp_dir).unwrap();
        std::fs::create_dir_all(&forbidden_dir).unwrap();

        let input_file = temp_dir.join("input.mp3");
        File::create(&input_file)
            .unwrap()
            .write_all(b"dummy")
            .unwrap();

        // Create a dangling symlink inside the allowed dir pointing to a file in the forbidden dir
        let forbidden_target = forbidden_dir.join("target.mp3");
        // Ensure forbidden_target does not exist so it is a dangling symlink
        if forbidden_target.exists() {
            std::fs::remove_file(&forbidden_target).unwrap();
        }

        let symlink_path = temp_dir.join("link.mp3");
        if symlink_path.exists() || std::fs::symlink_metadata(&symlink_path).is_ok() {
            std::fs::remove_file(&symlink_path).unwrap();
        }
        symlink(&forbidden_target, &symlink_path).unwrap();

        let processor = MediaProcessor::new(vec![&temp_dir]);

        // This should fail because it is a symlink, even if dangling
        let result = processor
            .convert_audio(&input_file, &symlink_path, "mp3")
            .await;

        match result {
            Err(Error::Security(msg)) => {
                if !msg.contains("Output path cannot be a symlink") {
                    panic!("Unexpected security error message: {}", msg);
                }
            }
            Ok(_) => panic!("Dangling symlink output was allowed!"),
            Err(e) => panic!("Unexpected error type: {:?}", e),
        }

        std::fs::remove_dir_all(&temp_dir).unwrap();
        std::fs::remove_dir_all(&forbidden_dir).unwrap();
        Ok(())
    }
}
