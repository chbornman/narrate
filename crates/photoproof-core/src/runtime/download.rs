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
            throttle_sleep: Duration::from_millis(50),
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
            connect_timeout: Duration::from_secs(5),
            read_timeout: Duration::from_secs(30),
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
                let dest = dir.join(f.file_name());
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
        let dir = self.models_dir.join(&model.id);
        std::fs::create_dir_all(&dir)?;
        let mut outcome = DownloadOutcome {
            files_fetched: 0,
            bytes_fetched: 0,
        };
        for file in &model.files {
            let dest = dir.join(file.file_name());
            if std::fs::metadata(&dest).map(|m| m.len()).ok() == Some(file.bytes) {
                continue; // verified at rename time on a previous run
            }
            match self.fetch_and_verify(model, file, &dest, pacer) {
                Ok(bytes) => {
                    outcome.files_fetched += 1;
                    outcome.bytes_fetched += bytes;
                }
                Err(DownloadError::ChecksumFailed { .. }) => {
                    // §5.2: mismatch deletes and re-downloads — ONE
                    // automatic retry, then surface.
                    let _ = std::fs::remove_file(part_path(&dest));
                    let _ = std::fs::remove_file(&dest);
                    match self.fetch_and_verify(model, file, &dest, pacer) {
                        Ok(bytes) => {
                            outcome.files_fetched += 1;
                            outcome.bytes_fetched += bytes;
                        }
                        Err(e) => return Err(e),
                    }
                }
                Err(e) => return Err(e),
            }
        }
        // All files verified ⇒ installed (§5.2 atomicity at model grain).
        self.record_installed(&model.id, manifest_version, now_rfc3339)?;
        Ok(outcome)
    }

    fn fetch_and_verify(
        &self,
        model: &ModelEntry,
        file: &FileEntry,
        dest: &Path,
        pacer: &mut dyn Pacer,
    ) -> Result<u64, DownloadError> {
        // B55 fail-closed pre-flight: an unpinned entry never reaches the
        // network — not even a HEAD. (The embedder entries ship this way
        // until spike session 2 pins them.)
        if file.sha256.bytes().all(|b| b == b'0') || file.revision == "UNPINNED-P6.3" {
            return Err(DownloadError::Unpinned {
                file: file.file_name().to_owned(),
            });
        }
        let part = part_path(dest);
        let mut have: u64 = std::fs::metadata(&part).map(|m| m.len()).unwrap_or(0);
        let url = file.url();
        if url.starts_with("https://") {
            return self.fetch_https(model, file, dest, &part, have, &url, pacer);
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
        let mut buf = [0u8; 64 * 1024];
        loop {
            match resp.read_some(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    out.write_all(&buf[..n])?;
                    have += n as u64;
                    fetched += n as u64;
                    pacer.pace(n);
                    // Coalesced progress (≥ 4 MB steps) on the bus.
                    if have - last_progress >= 4 * 1024 * 1024 {
                        last_progress = have;
                        self.bus.publish(RuntimeEvent::DownloadProgress {
                            model_id: model.id.clone(),
                            downloaded_bytes: have,
                            total_bytes: file.bytes,
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
        self.verify_and_finish(model, file, &part, dest)?;
        Ok(fetched)
    }

    /// The B66 TLS transport: `ureq` (rustls) for the manifest's real
    /// `hf:` entries — synchronous like the rest of this worker, follows
    /// the HF resolve→CDN redirect, honors Range resume. The stub-server
    /// `http://` path above stays on the localhost-grade client the §13
    /// acceptance tests drive.
    fn fetch_https(
        &self,
        model: &ModelEntry,
        file: &FileEntry,
        dest: &Path,
        part: &Path,
        mut have: u64,
        url: &str,
        pacer: &mut dyn Pacer,
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
        let mut buf = [0u8; 64 * 1024];
        loop {
            match reader.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    out.write_all(&buf[..n])?;
                    have += n as u64;
                    fetched += n as u64;
                    pacer.pace(n);
                    if have - last_progress >= 4 * 1024 * 1024 {
                        last_progress = have;
                        self.bus.publish(RuntimeEvent::DownloadProgress {
                            model_id: model.id.clone(),
                            downloaded_bytes: have,
                            total_bytes: file.bytes,
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
        self.verify_and_finish(model, file, part, dest)?;
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
    ) -> Result<(), DownloadError> {
        let digest = sha256_file(part)?;
        if digest != file.sha256.to_lowercase() {
            return Err(DownloadError::ChecksumFailed {
                file: file.file_name().to_owned(),
            });
        }
        std::fs::rename(part, dest)?;
        self.bus.publish(RuntimeEvent::DownloadProgress {
            model_id: model.id.clone(),
            downloaded_bytes: file.bytes,
            total_bytes: file.bytes,
        });
        Ok(())
    }
}

fn part_path(dest: &Path) -> PathBuf {
    let mut p = dest.as_os_str().to_owned();
    p.push(".part");
    PathBuf::from(p)
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
