//! The `photoproof://` custom URI scheme: thumbnails and Look images served
//! straight from the preview cache (spec/UI.md §3.3, DECISIONS P16).
//!
//! Image bytes NEVER cross `invoke`/IPC and are never base64-encoded. URLs
//! are content-addressed (`photoproof://localhost/{thumb|display}/{hash}`),
//! so responses carry immutable cache headers and the webview's own HTTP
//! cache does the rest.

use std::path::{Path, PathBuf};

use photoproof_core::ContentHash;
use photoproof_core::library::{ArtifactKind, artifact_path};

/// Parse `/thumb/<hash>` | `/display/<hash>` (a trailing `.webp` is
/// tolerated). Returns the artifact kind and the validated content hash.
pub fn parse_path(path: &str) -> Option<(ArtifactKind, ContentHash)> {
    let mut parts = path.trim_start_matches('/').splitn(2, '/');
    let kind = match parts.next()? {
        "thumb" => ArtifactKind::Thumb,
        "display" => ArtifactKind::Display,
        _ => return None,
    };
    let rest = parts.next()?;
    let hash_str = rest.strip_suffix(".webp").unwrap_or(rest);
    let hash = ContentHash::from_hex(hash_str).ok()?;
    Some((kind, hash))
}

/// Resolve a request path to the cached WebP file (existence-checked).
pub fn resolve(cache_dir: &Path, path: &str) -> Option<PathBuf> {
    let (kind, hash) = parse_path(path)?;
    let file = artifact_path(cache_dir, &hash, kind);
    file.exists().then_some(file)
}

pub fn respond_ok(bytes: Vec<u8>) -> http::Response<Vec<u8>> {
    http::Response::builder()
        .status(200)
        .header("content-type", "image/webp")
        // Content-addressed: immutable forever.
        .header("cache-control", "public, max-age=31536000, immutable")
        .body(bytes)
        .expect("static response")
}

pub fn respond_not_found() -> http::Response<Vec<u8>> {
    http::Response::builder()
        .status(404)
        .header("cache-control", "no-store")
        .body(Vec::new())
        .expect("static response")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hash() -> ContentHash {
        ContentHash::from_bytes_of(b"x")
    }

    #[test]
    fn parses_thumb_and_display_paths() {
        let h = hash();
        let (k, parsed) = parse_path(&format!("/thumb/{}", h.as_str())).unwrap();
        assert_eq!(k, ArtifactKind::Thumb);
        assert_eq!(parsed, h);
        let (k, _) = parse_path(&format!("/display/{}.webp", h.as_str())).unwrap();
        assert_eq!(k, ArtifactKind::Display);
    }

    #[test]
    fn rejects_unknown_kinds_bad_hashes_and_traversal() {
        let h = hash();
        assert!(parse_path(&format!("/full/{}", h.as_str())).is_none());
        assert!(parse_path("/thumb/not-a-hash").is_none());
        assert!(parse_path("/thumb/../../etc/passwd").is_none());
        assert!(parse_path("/thumb/").is_none());
        assert!(parse_path("/").is_none());
    }

    #[test]
    fn resolve_requires_existing_artifact() {
        let dir = tempfile::tempdir().unwrap();
        let h = hash();
        assert!(resolve(dir.path(), &format!("/thumb/{}", h.as_str())).is_none());
        let file = artifact_path(dir.path(), &h, ArtifactKind::Thumb);
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(&file, b"webp-bytes").unwrap();
        assert_eq!(
            resolve(dir.path(), &format!("/thumb/{}", h.as_str())),
            Some(file)
        );
    }
}
