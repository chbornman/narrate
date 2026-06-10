//! The `photoproof://` custom URI scheme: thumbnails and Look images served
//! straight from the preview cache (spec/UI.md §3.3, DECISIONS P16), plus
//! the progressive full-resolution route for Look (dogfood round 1):
//!
//!   /thumb/<hash>     cached WebP thumbnail
//!   /display/<hash>   cached WebP display preview (2560 px class)
//!   /original/<hash>  the ORIGINAL file — served ONLY for webview-decodable
//!                     stored formats (jpeg/png/webp, by the images-table
//!                     format column, never extension sniffing), resolved
//!                     through the library's best ONLINE path; offline or
//!                     missing files refuse with 404. RAW/TIFF/HEIC sources
//!                     404 here by design — Look keeps the display preview
//!                     silently (the full decode is the M1.5 backfill).
//!
//! Image bytes NEVER cross `invoke`/IPC and are never base64-encoded. URLs
//! are content-addressed, so every route carries the same immutable cache
//! headers and the webview's own HTTP cache does the rest.

use std::path::{Path, PathBuf};

use photoproof_core::ContentHash;
use photoproof_core::library::{ArtifactKind, ImageFormat, Library, artifact_path};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Route {
    /// Cached preview artifact (always WebP).
    Artifact(ArtifactKind),
    /// The original source file (allowlisted formats only).
    Original,
}

/// Parse `/thumb/<hash>` | `/display/<hash>` (a trailing `.webp` is
/// tolerated) | `/original/<hash>`. Returns the route and the validated
/// content hash — hash validation is the traversal guard.
pub fn parse_path(path: &str) -> Option<(Route, ContentHash)> {
    let mut parts = path.trim_start_matches('/').splitn(2, '/');
    let route = match parts.next()? {
        "thumb" => Route::Artifact(ArtifactKind::Thumb),
        "display" => Route::Artifact(ArtifactKind::Display),
        "original" => Route::Original,
        _ => return None,
    };
    let rest = parts.next()?;
    let hash_str = match route {
        Route::Artifact(_) => rest.strip_suffix(".webp").unwrap_or(rest),
        Route::Original => rest,
    };
    let hash = ContentHash::from_hex(hash_str).ok()?;
    Some((route, hash))
}

/// Resolve an ARTIFACT request path to the cached WebP (existence-checked).
pub fn resolve(cache_dir: &Path, path: &str) -> Option<PathBuf> {
    match parse_path(path)? {
        (Route::Artifact(kind), hash) => {
            let file = artifact_path(cache_dir, &hash, kind);
            file.exists().then_some(file)
        }
        (Route::Original, _) => None, // originals resolve through the library
    }
}

/// THE /original allowlist — by STORED format (the images-table column the
/// scan classified), never by sniffing the request or the file name. Only
/// formats every webview decodes natively are served; everything else
/// (RAW, TIFF, HEIC) stays on the display preview until the M1.5 backfill.
pub fn original_content_type(format: ImageFormat) -> Option<&'static str> {
    match format {
        ImageFormat::Jpeg => Some("image/jpeg"),
        ImageFormat::Png => Some("image/png"),
        ImageFormat::Webp => Some("image/webp"),
        ImageFormat::Tiff | ImageFormat::Heic | ImageFormat::Raw => None,
    }
}

/// Resolve an /original request through the library: stored-format
/// allowlist → best path (LIBRARY §3.1) → ONLINE only → existence check.
/// Any refusal is a uniform `None` (the protocol answers 404; the frontend
/// keeps the preview silently).
pub fn resolve_original(library: &Library, hash: &ContentHash) -> Option<(PathBuf, &'static str)> {
    let record = library.image(hash).ok().flatten()?;
    let content_type = original_content_type(record.format)?;
    let best = library.best_path(hash).ok().flatten()?;
    if !best.online {
        return None;
    }
    let mount = best.mount_point?;
    let file = if best.row.rel_path.is_empty() {
        PathBuf::from(mount)
    } else {
        Path::new(&mount).join(&best.row.rel_path)
    };
    file.exists().then_some((file, content_type))
}

/// Serve any `photoproof://` request path against the library (artifacts
/// resolve under its preview cache; originals through its path tables).
pub fn serve(library: &Library, path: &str) -> http::Response<Vec<u8>> {
    match parse_path(path) {
        Some((Route::Artifact(_), _)) => resolve(library.cache_dir(), path)
            .and_then(|file| std::fs::read(file).ok())
            .map(|bytes| respond_ok(bytes, "image/webp"))
            .unwrap_or_else(respond_not_found),
        Some((Route::Original, hash)) => resolve_original(library, &hash)
            .and_then(|(file, content_type)| {
                std::fs::read(file).ok().map(|bytes| (bytes, content_type))
            })
            .map(|(bytes, content_type)| respond_ok(bytes, content_type))
            .unwrap_or_else(respond_not_found),
        None => respond_not_found(),
    }
}

pub fn respond_ok(bytes: Vec<u8>, content_type: &'static str) -> http::Response<Vec<u8>> {
    http::Response::builder()
        .status(200)
        .header("content-type", content_type)
        // Content-addressed: immutable forever (every route).
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
    use std::sync::Arc;

    use photoproof_core::library::{
        FakeVolumeProbe, LibraryOptions, PlatformIdKind, ProbedVolume, ScanOptions,
    };

    use super::*;

    fn hash() -> ContentHash {
        ContentHash::from_bytes_of(b"x")
    }

    #[test]
    fn parses_thumb_and_display_paths() {
        let h = hash();
        let (r, parsed) = parse_path(&format!("/thumb/{}", h.as_str())).unwrap();
        assert_eq!(r, Route::Artifact(ArtifactKind::Thumb));
        assert_eq!(parsed, h);
        let (r, _) = parse_path(&format!("/display/{}.webp", h.as_str())).unwrap();
        assert_eq!(r, Route::Artifact(ArtifactKind::Display));
    }

    #[test]
    fn parses_the_original_route() {
        let h = hash();
        let (r, parsed) = parse_path(&format!("/original/{}", h.as_str())).unwrap();
        assert_eq!(r, Route::Original);
        assert_eq!(parsed, h);
        // No suffix tolerance on originals (content-addressed, no extension).
        assert!(parse_path(&format!("/original/{}.webp", h.as_str())).is_none());
        assert!(parse_path(&format!("/original/{}.jpg", h.as_str())).is_none());
    }

    #[test]
    fn rejects_unknown_kinds_bad_hashes_and_traversal() {
        let h = hash();
        assert!(parse_path(&format!("/full/{}", h.as_str())).is_none());
        assert!(parse_path("/thumb/not-a-hash").is_none());
        assert!(parse_path("/thumb/../../etc/passwd").is_none());
        assert!(parse_path("/original/not-a-hash").is_none());
        assert!(parse_path("/original/../../etc/passwd").is_none());
        assert!(parse_path("/original/").is_none());
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

    #[test]
    fn the_original_allowlist_is_stored_format_only() {
        assert_eq!(original_content_type(ImageFormat::Jpeg), Some("image/jpeg"));
        assert_eq!(original_content_type(ImageFormat::Png), Some("image/png"));
        assert_eq!(original_content_type(ImageFormat::Webp), Some("image/webp"));
        // Not webview-decodable: the preview stands (M1.5 backfill).
        assert_eq!(original_content_type(ImageFormat::Tiff), None);
        assert_eq!(original_content_type(ImageFormat::Heic), None);
        assert_eq!(original_content_type(ImageFormat::Raw), None);
    }

    // ---- /original against a real temp library ----------------------------

    fn probed(mount: &Path) -> ProbedVolume {
        ProbedVolume {
            mount_point: mount.to_path_buf(),
            platform_id: Some("uuid-protocol-test".into()),
            platform_kind: PlatformIdKind::LinuxFsUuid,
            label: Some("ShootDisk".into()),
            fs_type: Some("ext4".into()),
            capacity_bytes: Some(1 << 30),
            read_only_flag: false,
            is_system_root: false,
            coarse_mtime: false,
        }
    }

    /// Temp library with one scanned root holding one JPEG and one TIFF
    /// (garbage bytes — hashing and extension classification do not care).
    fn env() -> (tempfile::TempDir, Library, FakeVolumeProbe) {
        let tmp = tempfile::tempdir().expect("tempdir");
        let mount = tmp.path().join("mount");
        let dir = mount.join("shoot");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("IMG_0001.jpg"), b"jpeg-original-bytes").unwrap();
        std::fs::write(dir.join("IMG_0002.tif"), b"tiff-original-bytes").unwrap();
        let probe = FakeVolumeProbe::new();
        probe.set_mounts(vec![probed(&mount)]);
        let lib = Library::open_with(
            tmp.path().join("photoproof.db"),
            tmp.path().join("previews"),
            LibraryOptions {
                probe: Arc::new(probe.clone()),
                ..Default::default()
            },
        )
        .unwrap();
        let root_id = lib.register_root(&dir, Some("shoot")).unwrap();
        lib.scan_root(&root_id, &ScanOptions::default()).unwrap();
        (tmp, lib, probe)
    }

    fn hash_with_format(lib: &Library, format: ImageFormat) -> ContentHash {
        lib.image_hashes()
            .unwrap()
            .into_iter()
            .find(|h| lib.image(h).unwrap().unwrap().format == format)
            .expect("scan ingested the format")
    }

    #[test]
    fn serves_the_original_with_its_stored_format_content_type() {
        let (_tmp, lib, _probe) = env();
        let h = hash_with_format(&lib, ImageFormat::Jpeg);
        let resp = serve(&lib, &format!("/original/{}", h.as_str()));
        assert_eq!(resp.status(), 200);
        assert_eq!(resp.headers()["content-type"], "image/jpeg");
        assert_eq!(
            resp.headers()["cache-control"],
            "public, max-age=31536000, immutable"
        );
        assert_eq!(resp.body().as_slice(), b"jpeg-original-bytes");
    }

    #[test]
    fn refuses_off_allowlist_stored_formats_with_404() {
        let (_tmp, lib, _probe) = env();
        let h = hash_with_format(&lib, ImageFormat::Tiff);
        // Online and present on disk — refused purely by stored format.
        let resp = serve(&lib, &format!("/original/{}", h.as_str()));
        assert_eq!(resp.status(), 404);
    }

    #[test]
    fn refuses_offline_volumes_and_unknown_hashes_with_404() {
        let (_tmp, lib, probe) = env();
        let h = hash_with_format(&lib, ImageFormat::Jpeg);
        probe.set_mounts(vec![]); // disk pulled
        lib.probe_volumes().unwrap();
        assert_eq!(
            serve(&lib, &format!("/original/{}", h.as_str())).status(),
            404
        );
        // Unknown (never-ingested) hash.
        assert_eq!(
            serve(&lib, &format!("/original/{}", hash().as_str())).status(),
            404
        );
    }
}
