//! The download manager (spec/RUNTIME.md §5.2–5.3), stub-verified
//! against a local HTTP server with cuttable connections:
//!
//! - **Resumable**: HTTP `Range` into `{file}.part`; on relaunch, resume
//!   from the part-file length. SHA-256 over the complete file; mismatch
//!   deletes and re-downloads — ONE automatic retry, then surface.
//! - **Atomic**: verified file renamed into place; a model is installed
//!   only when ALL its files verify (`installed.json`).
//! - **Concurrency**: one file at a time, background priority, throttled
//!   while a capture session is live (the [`Pacer`] seam).
//! - **THE LICENSE GATE (§13.7)**: zero bytes of an
//!   `acceptance_required` model move before the recorded acceptance
//!   exists — the gate sits BEFORE any request is issued, and the test
//!   asserts at the stub server.
//!
//! Transports: plain HTTP to explicit hosts (stub servers, LAN mirrors —
//! the §13 acceptance tests drive this path) and **https via ureq/rustls
//! (B66)** for the manifest's real `hf:` entries — synchronous like the
//! rest of this worker, follows the HF resolve→CDN redirect, honors
//! Range resume. Unpinned entries (B55 placeholders) are refused before
//! any byte moves; proven against the real network by the ignored
//! `real_https_fetch_verifies_the_smallest_pin` test.

use std::collections::BTreeMap;
use std::io::Write;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use photoproof_connectors::http::{self, HttpError};

use super::bus::{RuntimeBus, RuntimeEvent};
use super::manifest::{Acceptances, FileEntry, ModelEntry};

/// Transfer chunk size, shared by both transports. Load-bearing twice:
/// `pace()` runs once per chunk (so chunk size times throttle sleep sets
/// the throttled bandwidth), and progress coalescing advances in these
/// units. One constant keeps the plain-HTTP and TLS paths provably
/// identical.
const TRANSFER_CHUNK: usize = 64 * 1024;

/// Per-chunk sleep while a capture session is live (§5.2 background
/// priority). With the 64 KiB [`TRANSFER_CHUNK`] this caps throttled
/// throughput at ~1.3 MB/s — slow enough to stay out of capture's way,
/// fast enough that a model still arrives. The spec mandates the
/// throttle itself, not a user-tunable rate, so this is a constant and
/// not a config knob.
const CAPTURE_THROTTLE_SLEEP: Duration = Duration::from_millis(50);

/// HTTP timeout posture, one judgement in two numbers: connects fail
/// fast so the resume/retry machinery kicks in quickly, while reads
/// tolerate slow model CDNs — a 30 s stall is where "slow mirror" ends
/// and "resumable interruption" begins. Both transports consume the
/// pair (the localhost client and the ureq agent).
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const READ_TIMEOUT: Duration = Duration::from_secs(30);

/// Coalescing step for `DownloadProgress` bus events: publish only every
/// this-many bytes, because per-chunk publishing would flood the bus and
/// the UI on multi-GB model files. The published numbers are
/// MODEL-cumulative (bytes of this model on disk over its manifest
/// total), never per-file: DFN5B is ~400 files, mostly tiny shards, and a
/// per-file numerator against the model total read ~0% in settings while
/// gigabytes sat verified on disk (founder dogfood, June 2026).
const PROGRESS_STEP_BYTES: u64 = 4 * 1024 * 1024;

#[derive(Debug, thiserror::Error)]
pub enum DownloadError {
    /// §13.7: the request was never issued.
    #[error("license acceptance required for {model_id} before any byte moves")]
    LicenseNotAccepted { model_id: String },
    /// Connection cut mid-transfer; the part file holds the progress —
    /// retry/relaunch resumes from its length.
    #[error("transfer interrupted at {got_bytes} bytes (resumable)")]
    Interrupted { got_bytes: u64 },
    /// SHA-256 mismatch after the automatic re-fetch — surfaced to
    /// settings/debug panel (§5.2).
    #[error("checksum mismatch for {file} after automatic re-fetch")]
    ChecksumFailed { file: String },
    /// B55 fail-closed PRE-FLIGHT: a manifest entry still carrying the
    /// UNPINNED placeholder (all-zero sha / UNPINNED revision) is refused
    /// BEFORE any byte moves — embedder entries stay in this state until
    /// spike session 2 pins them.
    #[error("manifest entry for {file} is unpinned (UNPINNED-P6.3); refusing to fetch")]
    Unpinned { file: String },
    /// Path-preserving downloads (decision 1) join `file.path` verbatim
    /// under `models_dir/<id>/`. Before this lane, basename-only joins made
    /// escape structurally impossible; now an absolute, `..`-bearing, or
    /// empty path would write OUTSIDE the model dir and corrupt a sibling
    /// model (which remove_model/GC could never reclaim, since both delete
    /// only models_dir/<id>). Manifest is compiled-in, so this is a
    /// pre-flight hardening of the L2-generated DFN5B enumeration against a
    /// generator bug, not a live hole — but the containment invariant is
    /// now enforced here, not by construction.
    #[error("manifest entry path {file} is not a relative in-dir path; refusing to fetch")]
    UnsafePath { file: String },
    #[error("backend answered {status} for {url}")]
    Http { status: u16, url: String },
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("bad url: {0}")]
    BadUrl(String),
}

/// Background-priority throttle seam (§5.2: throttled while a capture
/// session is live). Called between chunks with the transferred size.
pub trait Pacer: Send {
    fn pace(&mut self, just_transferred: usize);
}

/// Production pacer: sleeps a beat per chunk while the capture-live flag
/// is set (the one honest sleep — IO pacing, not decision logic).
pub struct SleepPacer {
    capture_live: Arc<AtomicBool>,
    throttle_sleep: Duration,
}

impl SleepPacer {
    pub fn new(capture_live: Arc<AtomicBool>) -> Self {
        Self {
            capture_live,
            throttle_sleep: CAPTURE_THROTTLE_SLEEP,
        }
    }
}

impl Pacer for SleepPacer {
    fn pace(&mut self, _just_transferred: usize) {
        if self.capture_live.load(Ordering::Relaxed) {
            std::thread::sleep(self.throttle_sleep);
        }
    }
}

/// `installed.json`: model_id → {manifest_version, when} (§5.2).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InstalledRecord {
    pub manifest_version: u32,
    pub when: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DownloadOutcome {
    pub files_fetched: usize,
    pub bytes_fetched: u64,
}

pub struct DownloadManager {
    models_dir: PathBuf,
    bus: RuntimeBus,
    connect_timeout: Duration,
    read_timeout: Duration,
}

impl DownloadManager {
    pub fn new(models_dir: PathBuf, bus: RuntimeBus) -> Self {
        Self {
            models_dir,
            bus,
            connect_timeout: CONNECT_TIMEOUT,
            read_timeout: READ_TIMEOUT,
        }
    }

    pub fn models_dir(&self) -> &Path {
        &self.models_dir
    }

    fn installed_path(&self) -> PathBuf {
        self.models_dir.join("installed.json")
    }

    pub fn installed(&self) -> BTreeMap<String, InstalledRecord> {
        std::fs::read(self.installed_path())
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default()
    }

    pub fn is_installed(&self, model_id: &str) -> bool {
        self.installed().contains_key(model_id)
    }

    fn record_installed(
        &self,
        model_id: &str,
        manifest_version: u32,
        when: &str,
    ) -> std::io::Result<()> {
        let mut all = self.installed();
        all.insert(
            model_id.to_owned(),
            InstalledRecord {
                manifest_version,
                when: when.to_owned(),
            },
        );
        let tmp = self.installed_path().with_extension("json.tmp");
        std::fs::create_dir_all(&self.models_dir)?;
        std::fs::write(
            &tmp,
            serde_json::to_vec_pretty(&all).expect("installed json"),
        )?;
        std::fs::rename(tmp, self.installed_path())
    }

    pub fn remove_installed_record(&self, model_id: &str) -> std::io::Result<()> {
        let mut all = self.installed();
        all.remove(model_id);
        let tmp = self.installed_path().with_extension("json.tmp");
        std::fs::write(
            &tmp,
            serde_json::to_vec_pretty(&all).expect("installed json"),
        )?;
        std::fs::rename(tmp, self.installed_path())
    }

    /// Bytes already on disk for a model (final + part files) — settings
    /// progress.
    pub fn downloaded_bytes(&self, model: &ModelEntry) -> u64 {
        let dir = self.models_dir.join(&model.id);
        model
            .files
            .iter()
            .map(|f| {
                // Path-preserving: join the FULL relative path, not the
                // basename. DFN5B ships visual/model.onnx AND
                // textual/model.onnx — basename collision would double-count
                // one and miss the other. Flat entries (path == basename)
                // resolve identically, so installed models are unaffected.
                let dest = dir.join(&f.path);
                let part = part_path(&dest);
                std::fs::metadata(&dest)
                    .or_else(|_| std::fs::metadata(&part))
                    .map(|m| m.len())
                    .unwrap_or(0)
            })
            .sum()
    }

    /// Download (or finish downloading) one model: license gate first,
    /// then file-by-file — one at a time (§5.2). On full verification the
    /// model is recorded installed.
    pub fn download_model(
        &self,
        model: &ModelEntry,
        manifest_version: u32,
        acceptances: &Acceptances,
        pacer: &mut dyn Pacer,
        now_rfc3339: &str,
    ) -> Result<DownloadOutcome, DownloadError> {
        // THE LICENSE GATE — before any socket is opened, any request
        // issued, any byte moved (§13.7 asserts at the server).
        if !acceptances.permits(model) {
            return Err(DownloadError::LicenseNotAccepted {
                model_id: model.id.clone(),
            });
        }
        // Containment pre-flight (decision 1 made dest = dir.join(path),
        // which Path::join lets escape via an absolute or `..` component).
        // Reject the whole model before creating a directory or moving a
        // byte: one bad path would corrupt a sibling install. Checked here,
        // once, ahead of any side effect.
        for file in &model.files {
            if !is_contained_relative_path(&file.path) {
                return Err(DownloadError::UnsafePath {
                    file: file.path.clone(),
                });
            }
        }
        let dir = self.models_dir.join(&model.id);
        std::fs::create_dir_all(&dir)?;
        let mut outcome = DownloadOutcome {
            files_fetched: 0,
            bytes_fetched: 0,
        };
        // Model-cumulative progress baseline: bytes of files BEFORE the
        // in-flight one, already verified-complete (skipped below or just
        // fetched). Files transfer strictly in manifest order, so prior
        // files are always whole — `base + have` is exactly "bytes of this
        // model on disk", the one meaning the settings row displays.
        let mut base: u64 = 0;
        for file in &model.files {
            // Path-preserving dest under models_dir/<model_id>/<file.path>.
            // The subdirectory layout is load-bearing: ort resolves the
            // DFN5B visual tower's ~100 external-data files RELATIVE to
            // visual/model.onnx, so the part-file, the verified rename, and
            // the final on-disk file all live at the nested path. Flat
            // entries keep path == basename, so their layout is unchanged.
            let dest = dir.join(&file.path);
            // Create the parent for nested paths before any part file is
            // opened (flat paths join to `dir`, already created above).
            if let Some(parent) = dest.parent() {
                std::fs::create_dir_all(parent)?;
            }
            if std::fs::metadata(&dest).map(|m| m.len()).ok() == Some(file.bytes) {
                base += file.bytes;
                continue; // verified at rename time on a previous run
            }
            match self.fetch_and_verify(model, file, &dest, pacer, base) {
                Ok(bytes) => {
                    outcome.files_fetched += 1;
                    outcome.bytes_fetched += bytes;
                }
                Err(DownloadError::ChecksumFailed { .. }) => {
                    // §5.2: mismatch deletes and re-downloads — ONE
                    // automatic retry, then surface.
                    let _ = std::fs::remove_file(part_path(&dest));
                    let _ = std::fs::remove_file(&dest);
                    match self.fetch_and_verify(model, file, &dest, pacer, base) {
                        Ok(bytes) => {
                            outcome.files_fetched += 1;
                            outcome.bytes_fetched += bytes;
                        }
                        Err(e) => return Err(e),
                    }
                }
                Err(e) => return Err(e),
            }
            // Errors returned above; reaching here means the file verified
            // and renamed, so it joins the cumulative baseline.
            base += file.bytes;
        }
        // All files verified ⇒ installed (§5.2 atomicity at model grain).
        self.record_installed(&model.id, manifest_version, now_rfc3339)?;
        Ok(outcome)
    }

    /// `base` is the model-cumulative byte count of the files already
    /// complete before this one — every progress event publishes
    /// `base + have` against `model.total_bytes` so the bus always speaks
    /// in model terms (see [`PROGRESS_STEP_BYTES`]).
    fn fetch_and_verify(
        &self,
        model: &ModelEntry,
        file: &FileEntry,
        dest: &Path,
        pacer: &mut dyn Pacer,
        base: u64,
    ) -> Result<u64, DownloadError> {
        // B55 fail-closed pre-flight: an unpinned entry never reaches the
        // network — not even a HEAD. (The embedder entries ship this way
        // until spike session 2 pins them.)
        if !file.is_pinned() {
            // Identify by the full relative path, not the basename: DFN5B
            // ships visual/model.onnx AND textual/model.onnx (and L2 pins
            // ~100 same-basename external-data shards across the two
            // towers), so the basename alone cannot tell the founder which
            // file is unpinned in the debug panel / download_errors.
            return Err(DownloadError::Unpinned {
                file: file.path.clone(),
            });
        }
        let part = part_path(dest);
        let mut have: u64 = std::fs::metadata(&part).map(|m| m.len()).unwrap_or(0);
        // A byte-complete part — a quit landed between the last byte and
        // the verify (founder dogfood, June 2026) — goes straight to
        // verification: a Range from EOF draws 416 from real CDNs and the
        // retry loop can never finish. An over-long part is corrupt;
        // restart clean.
        if have > file.bytes {
            let _ = std::fs::remove_file(&part);
            have = 0;
        } else if have == file.bytes && file.bytes > 0 {
            self.verify_and_finish(model, file, &part, dest, base)?;
            return Ok(0);
        }
        let url = file.url();
        if url.starts_with("https://") {
            return self.fetch_https(model, file, dest, &part, have, &url, pacer, base);
        }
        let (addr, path) = parse_http_url(&url)?;
        let mut headers: Vec<(String, String)> = Vec::new();
        if have > 0 {
            headers.push(("Range".into(), format!("bytes={have}-")));
        }
        let (mut resp, _disconnect) = http::request(
            addr,
            "GET",
            &path,
            &headers,
            None,
            self.connect_timeout,
            self.read_timeout,
        )
        .map_err(|e| match e {
            HttpError::ConnectionLost(_) | HttpError::TimedOut => {
                DownloadError::Interrupted { got_bytes: have }
            }
            other => DownloadError::Http {
                status: 0,
                url: format!("{url} ({other})"),
            },
        })?;
        match resp.status {
            206 => {}
            200 => {
                if have > 0 {
                    // Server ignored the Range: restart clean.
                    let _ = std::fs::remove_file(&part);
                    have = 0;
                }
            }
            status => return Err(DownloadError::Http { status, url }),
        }
        let mut out = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&part)?;
        let mut fetched: u64 = 0;
        let mut last_progress = have;
        let mut buf = [0u8; TRANSFER_CHUNK];
        loop {
            match resp.read_some(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    out.write_all(&buf[..n])?;
                    have += n as u64;
                    fetched += n as u64;
                    pacer.pace(n);
                    // Coalesced progress on the bus, in MODEL-cumulative
                    // terms (the only meaning the settings row displays).
                    if have - last_progress >= PROGRESS_STEP_BYTES {
                        last_progress = have;
                        self.bus.publish(RuntimeEvent::DownloadProgress {
                            model_id: model.id.clone(),
                            downloaded_bytes: base + have,
                            total_bytes: model.total_bytes,
                        });
                    }
                }
                Err(HttpError::ConnectionLost(_) | HttpError::TimedOut) => {
                    out.flush()?;
                    return Err(DownloadError::Interrupted { got_bytes: have });
                }
                Err(e) => {
                    out.flush()?;
                    return Err(DownloadError::Http {
                        status: 0,
                        url: format!("{url} ({e})"),
                    });
                }
            }
        }
        out.flush()?;
        drop(out);
        self.verify_and_finish(model, file, &part, dest, base)?;
        Ok(fetched)
    }

    /// The B66 TLS transport: `ureq` (rustls) for the manifest's real
    /// `hf:` entries — synchronous like the rest of this worker, follows
    /// the HF resolve→CDN redirect, honors Range resume. The stub-server
    /// `http://` path above stays on the localhost-grade client the §13
    /// acceptance tests drive.
    // The argument list mirrors the transfer's full resume context; a
    // params struct would only rename the coupling.
    #[allow(clippy::too_many_arguments)]
    fn fetch_https(
        &self,
        model: &ModelEntry,
        file: &FileEntry,
        dest: &Path,
        part: &Path,
        mut have: u64,
        url: &str,
        pacer: &mut dyn Pacer,
        base: u64,
    ) -> Result<u64, DownloadError> {
        use std::io::Read;
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .timeout_connect(Some(self.connect_timeout))
            .timeout_recv_response(Some(self.read_timeout))
            .build()
            .into();
        let mut req = agent.get(url);
        if have > 0 {
            req = req.header("Range", &format!("bytes={have}-"));
        }
        let resp = match req.call() {
            Ok(r) => r,
            Err(ureq::Error::StatusCode(status)) => {
                return Err(DownloadError::Http {
                    status,
                    url: url.into(),
                });
            }
            // Transport-class failures (DNS, connect, TLS, mid-read cuts)
            // are all resumable from the part file's length.
            Err(_) => return Err(DownloadError::Interrupted { got_bytes: have }),
        };
        if resp.status() == 200 && have > 0 {
            // Server ignored the Range: restart clean.
            let _ = std::fs::remove_file(part);
            have = 0;
        }
        let mut out = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(part)?;
        let mut reader = resp.into_body().into_reader();
        let mut fetched: u64 = 0;
        let mut last_progress = have;
        let mut buf = [0u8; TRANSFER_CHUNK];
        loop {
            match reader.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    out.write_all(&buf[..n])?;
                    have += n as u64;
                    fetched += n as u64;
                    pacer.pace(n);
                    if have - last_progress >= PROGRESS_STEP_BYTES {
                        last_progress = have;
                        self.bus.publish(RuntimeEvent::DownloadProgress {
                            model_id: model.id.clone(),
                            downloaded_bytes: base + have,
                            total_bytes: model.total_bytes,
                        });
                    }
                }
                Err(_) => {
                    out.flush()?;
                    return Err(DownloadError::Interrupted { got_bytes: have });
                }
            }
        }
        out.flush()?;
        drop(out);
        self.verify_and_finish(model, file, part, dest, base)?;
        Ok(fetched)
    }

    /// Verify SHA-256 over the COMPLETE file, then atomic rename — shared
    /// tail of both transports (§5.2 atomicity at file grain).
    fn verify_and_finish(
        &self,
        model: &ModelEntry,
        file: &FileEntry,
        part: &Path,
        dest: &Path,
        base: u64,
    ) -> Result<(), DownloadError> {
        let digest = sha256_file(part)?;
        if digest != file.sha256.to_lowercase() {
            // The bytes are wrong; keeping them would re-fail every retry
            // (resume Ranges past them, or the complete-part fast path
            // re-verifies the same corruption). Restart clean.
            let _ = std::fs::remove_file(part);
            // Full relative path, not basename — same-basename files (DFN5B
            // visual/ vs textual/, L2 external-data shards) would otherwise
            // surface an ambiguous "checksum mismatch for model.onnx".
            return Err(DownloadError::ChecksumFailed {
                file: file.path.clone(),
            });
        }
        std::fs::rename(part, dest)?;
        // Per-file completion event, still in model-cumulative terms — for
        // DFN5B's ~290 sub-4 MB shards this is the ONLY event each file
        // emits (the coalescing step never trips), so it is what actually
        // advances the settings row through the long shard stretch.
        self.bus.publish(RuntimeEvent::DownloadProgress {
            model_id: model.id.clone(),
            downloaded_bytes: base + file.bytes,
            total_bytes: model.total_bytes,
        });
        Ok(())
    }
}

fn part_path(dest: &Path) -> PathBuf {
    let mut p = dest.as_os_str().to_owned();
    p.push(".part");
    PathBuf::from(p)
}

/// True only when joining `rel` to a model dir stays inside it: every
/// component must be a plain name. We reject empties and absolutes (which
/// Path::join would let REPLACE the base), `..` (traversal up and out),
/// `.` and a root/prefix (Windows drive) — anything that is not a normal
/// in-directory segment. Manifest entries are flat names or forward-slash
/// nested paths (`visual/model.onnx`), so this passes the real pins and
/// fails only a malformed path. Used as a download pre-flight to keep the
/// containment invariant the basename-only join used to give for free.
fn is_contained_relative_path(rel: &str) -> bool {
    if rel.is_empty() {
        return false;
    }
    // Treat backslashes as separators too: a `..\x` smuggled past a
    // forward-slash-only check must not reach Path::join on Windows.
    let p = Path::new(rel);
    p.components()
        .all(|c| matches!(c, std::path::Component::Normal(_)))
        && !rel.contains('\\')
}

fn parse_http_url(url: &str) -> Result<(SocketAddr, String), DownloadError> {
    let rest = url
        .strip_prefix("http://")
        .ok_or_else(|| DownloadError::BadUrl(url.into()))?;
    let (hostport, path) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, "/"),
    };
    let addr = std::net::ToSocketAddrs::to_socket_addrs(&hostport)
        .map_err(|e| DownloadError::BadUrl(format!("{url}: {e}")))?
        .next()
        .ok_or_else(|| DownloadError::BadUrl(url.into()))?;
    Ok((addr, path.to_owned()))
}

pub fn sha256_file(path: &Path) -> std::io::Result<String> {
    let mut hasher = Sha256::new();
    let mut file = std::fs::File::open(path)?;
    std::io::copy(&mut file, &mut hasher)?;
    Ok(hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect())
}

pub fn sha256_bytes(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// A no-throttle pacer for tests and non-capture contexts.
pub struct NoPace;

impl Pacer for NoPace {
    fn pace(&mut self, _just_transferred: usize) {}
}
