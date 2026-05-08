//! Content-type bucket detection. Header is the primary signal; magic bytes
//! confirm or override (servers lie about binary blobs as
//! `application/octet-stream`).

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Bucket {
    Image,
    Pdf,
    Audio,
    Video,
    Text,
}

/// Best-effort bucket selection using `Content-Type` header first, then magic
/// bytes from `infer`. Returns `None` if neither maps to a supported bucket.
pub fn detect(content_type_header: &str, leading_bytes: &[u8]) -> Option<Bucket> {
    let media_type = content_type_header
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();

    if let Some(b) = bucket_from_media_type(&media_type) {
        return Some(b);
    }

    let kind = infer::get(leading_bytes)?;
    bucket_from_media_type(kind.mime_type())
}

fn bucket_from_media_type(mime: &str) -> Option<Bucket> {
    match mime {
        "image/png" | "image/jpeg" | "image/jpg" | "image/webp" | "image/gif" => {
            Some(Bucket::Image)
        }
        "application/pdf" => Some(Bucket::Pdf),
        m if m.starts_with("audio/") => Some(Bucket::Audio),
        m if m.starts_with("video/") => Some(Bucket::Video),
        "text/html"
        | "application/xhtml+xml"
        | "application/xml"
        | "text/xml"
        | "text/plain"
        | "application/json" => Some(Bucket::Text),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_image_jpeg_returns_image() {
        assert_eq!(
            detect("image/jpeg; charset=binary", &[]),
            Some(Bucket::Image)
        );
    }

    #[test]
    fn header_lies_magic_corrects_to_pdf() {
        let bytes = b"%PDF-1.7\n%...";
        assert_eq!(detect("application/octet-stream", bytes), Some(Bucket::Pdf));
    }

    #[test]
    fn unknown_returns_none() {
        assert_eq!(detect("application/x-bogus", &[0x00, 0x01]), None);
    }

    #[test]
    fn header_text_html_returns_text() {
        assert_eq!(detect("text/html; charset=utf-8", &[]), Some(Bucket::Text));
    }

    #[test]
    fn header_missing_magic_png_returns_image() {
        let png = [0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A];
        assert_eq!(detect("", &png), Some(Bucket::Image));
    }

    #[test]
    fn audio_mp3_via_header() {
        assert_eq!(detect("audio/mpeg", &[]), Some(Bucket::Audio));
    }

    #[test]
    fn video_mp4_via_header() {
        assert_eq!(detect("video/mp4", &[]), Some(Bucket::Video));
    }
}
