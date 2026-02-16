use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum MediaType {
    Image,
    Audio,
    Video,
    Document,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum MediaFormat {
    Png,
    Jpeg,
    Gif,
    Webp,
    Mp3,
    Ogg,
    Wav,
    Mp4,
    Webm,
    Pdf,
    Other(String),
}

impl MediaFormat {
    pub fn from_extension(ext: &str) -> Self {
        match ext.to_lowercase().as_str() {
            "png" => Self::Png,
            "jpg" | "jpeg" => Self::Jpeg,
            "gif" => Self::Gif,
            "webp" => Self::Webp,
            "mp3" => Self::Mp3,
            "ogg" => Self::Ogg,
            "wav" => Self::Wav,
            "mp4" => Self::Mp4,
            "webm" => Self::Webm,
            "pdf" => Self::Pdf,
            other => Self::Other(other.to_string()),
        }
    }

    pub fn mime_type(&self) -> &str {
        match self {
            Self::Png => "image/png",
            Self::Jpeg => "image/jpeg",
            Self::Gif => "image/gif",
            Self::Webp => "image/webp",
            Self::Mp3 => "audio/mpeg",
            Self::Ogg => "audio/ogg",
            Self::Wav => "audio/wav",
            Self::Mp4 => "video/mp4",
            Self::Webm => "video/webm",
            Self::Pdf => "application/pdf",
            Self::Other(_) => "application/octet-stream",
        }
    }
}
