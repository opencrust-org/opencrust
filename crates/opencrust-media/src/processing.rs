use opencrust_common::Result;

/// Media processing pipeline for images, audio, and video.
///
/// Wraps external tools (ffmpeg, etc.) and provides a consistent API
/// for media operations needed by channels and agents.
pub struct MediaProcessor;

impl MediaProcessor {
    pub fn new() -> Self {
        Self
    }

    /// Check if ffmpeg is available on the system.
    pub fn ffmpeg_available() -> bool {
        std::process::Command::new("ffmpeg")
            .arg("-version")
            .output()
            .is_ok()
    }

    /// Convert audio using ffmpeg.
    pub async fn convert_audio(
        &self,
        input: &std::path::Path,
        output: &std::path::Path,
    ) -> Result<()> {
        let status = tokio::process::Command::new("ffmpeg")
            .args([
                "-i",
                &input.to_string_lossy(),
                "-y",
                &output.to_string_lossy(),
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

impl Default for MediaProcessor {
    fn default() -> Self {
        Self::new()
    }
}
