//! Minimal app-side settings persistence. UI preferences (sort per folder,
//! thumb size, rail pin) live in webview localStorage; state the Rust side
//! owns lands here: the last-export timestamp shown inline in Settings →
//! Export (spec/UI.md §2.4) and the stacked-pair display preference —
//! edited in the Settings window, consumed live by the main window, so it
//! needs a store both webviews share.

use std::collections::BTreeMap;
#[cfg(not(target_os = "windows"))]
use std::fs::File;
use std::fs::OpenOptions;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

/// Which member a collapsed RAW+JPEG stack DISPLAYS (featureset §5 dogfood
/// amendment: "Stacked pairs show: JPEG (default) | RAW"). The frontend's
/// stacks.ts display-member selection and the Look R-flip starting member
/// follow this.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum StackDisplay {
    #[default]
    Jpeg,
    Raw,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ProcessingIntensity {
    Eco,
    #[default]
    Balanced,
    Max,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum NewRootPolicy {
    #[default]
    ProcessNow,
    PreviewOnly,
    ProcessLater,
}

/// Default 1:1 preview cache budget: 20 GB. Generous on purpose — the user
/// REVIEWS (opens many RAWs, builds many full-res 1:1s) and should not babysit
/// storage (DESIGN-PREVIEW-POLICY.md). The evictor only bites past this, and
/// even then SAFELY (a discarded 1:1 re-derives on next view).
pub const DEFAULT_PREVIEW_CACHE_BUDGET_BYTES: u64 = 20 * 1024 * 1024 * 1024;

/// serde `default` for `preview_cache_budget_bytes`: a settings.json written
/// before this field existed (or with the key absent) loads the 20 GB default
/// rather than 0 — a 0 budget would otherwise evict the entire 1:1 cache on the
/// first develop.
fn default_preview_cache_budget_bytes() -> u64 {
    DEFAULT_PREVIEW_CACHE_BUDGET_BYTES
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct AppSettings {
    pub last_export_ts: Option<String>,
    pub stack_display: StackDisplay,
    /// Configurable external editor (BACKLOG "Configurable external editor,
    /// D4 revisit"): the app name (macOS) or executable (Win/Linux) the
    /// "Open in external editor" verb hands the ORIGINAL off to. None = use
    /// the OS default handler, so the single menu seat always does
    /// something sensible. `#[serde(default)]` (the struct attr above) lets
    /// pre-existing settings.json files — written before this field — load
    /// it as None instead of failing to parse.
    pub external_editor: Option<String>,
    /// 1:1 preview cache budget in BYTES (DESIGN-PREVIEW-POLICY.md): keep
    /// full-res 1:1 develop artifacts until the on-disk cache exceeds this,
    /// then evict least-recently-viewed. The one knob the policy exposes — the
    /// small thumb/display tiers are always kept and are not governed by it.
    /// Stored in bytes (the UI edits GB); defaults to 20 GB via
    /// `default_preview_cache_budget_bytes` so a legacy/absent key never reads
    /// as a 0 budget (which would evict everything).
    #[serde(default = "default_preview_cache_budget_bytes")]
    pub preview_cache_budget_bytes: u64,
    /// Shared CPU/RAM/I/O ceiling for background processing.
    pub processing_intensity: ProcessingIntensity,
    /// Explicit user pause. Interactive 1:1 development remains admitted.
    pub processing_paused: bool,
    /// Whether adding a source immediately starts its initial full walk.
    pub new_root_policy: NewRootPolicy,
    /// Per-source effective add policy. This freezes a one-shot override (and
    /// the default at add time) so changing the future default never silently
    /// changes an existing source's processing contract.
    pub root_processing_policies: BTreeMap<String, NewRootPolicy>,
    /// Independent model-pass switches. Pending rows remain durable and resume
    /// when re-enabled; no queue rewriting is required.
    pub defer_text_embeddings: bool,
    pub defer_image_embeddings: bool,
}

/// Manual `Default` (not derived): the budget defaults to 20 GB, not the `u64`
/// zero that a derive would give — a 0 budget evicts the whole 1:1 cache.
impl Default for AppSettings {
    fn default() -> Self {
        Self {
            last_export_ts: None,
            stack_display: StackDisplay::default(),
            external_editor: None,
            preview_cache_budget_bytes: DEFAULT_PREVIEW_CACHE_BUDGET_BYTES,
            processing_intensity: ProcessingIntensity::default(),
            processing_paused: false,
            new_root_policy: NewRootPolicy::default(),
            root_processing_policies: BTreeMap::new(),
            defer_text_embeddings: false,
            defer_image_embeddings: false,
        }
    }
}

pub fn settings_path(app_data: &Path) -> PathBuf {
    app_data.join("settings.json")
}

pub const CONTROL_FILE_POLL_INTERVAL: Duration = Duration::from_millis(40);
pub const CONTROL_FILE_DEBOUNCE: Duration = Duration::from_millis(160);
pub const CONTROL_FILE_QUARANTINE_RETENTION: usize = 5;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum LiveControlFile {
    Settings,
    Config,
    Tuning,
}

impl LiveControlFile {
    pub const ALL: [Self; 3] = [Self::Settings, Self::Config, Self::Tuning];

    pub fn name(self) -> &'static str {
        match self {
            Self::Settings => "settings",
            Self::Config => "config",
            Self::Tuning => "tuning",
        }
    }

    pub fn path(self, app_data: &Path) -> PathBuf {
        match self {
            Self::Settings => settings_path(app_data),
            Self::Config => app_data.join("config.toml"),
            Self::Tuning => app_data.join("tuning.toml"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LiveControlStatus {
    pub name: &'static str,
    pub last_attempted_at_ms: Option<u64>,
    pub last_applied_at_ms: Option<u64>,
    pub last_recovered_at_ms: Option<u64>,
    pub retained_error: Option<String>,
    pub recovery_source: Option<String>,
    pub quarantined: Vec<PathBuf>,
    pub warnings: Vec<String>,
}

impl LiveControlStatus {
    fn new(file: LiveControlFile) -> Self {
        Self {
            name: file.name(),
            last_attempted_at_ms: None,
            last_applied_at_ms: None,
            last_recovered_at_ms: None,
            retained_error: None,
            recovery_source: None,
            quarantined: Vec::new(),
            warnings: Vec::new(),
        }
    }
}

#[derive(Debug)]
pub struct LiveControlState {
    statuses: BTreeMap<LiveControlFile, LiveControlStatus>,
}

impl Default for LiveControlState {
    fn default() -> Self {
        Self {
            statuses: LiveControlFile::ALL
                .into_iter()
                .map(|file| (file, LiveControlStatus::new(file)))
                .collect(),
        }
    }
}

impl LiveControlState {
    pub fn snapshot(&self) -> Vec<LiveControlStatus> {
        self.statuses.values().cloned().collect()
    }

    pub fn begin_attempt(&mut self, file: LiveControlFile) {
        self.statuses
            .get_mut(&file)
            .expect("live control status")
            .last_attempted_at_ms = Some(system_time_ms(SystemTime::now()));
    }

    pub fn applied(
        &mut self,
        file: LiveControlFile,
        recovery_source: impl Into<String>,
        quarantined: Vec<PathBuf>,
        warnings: Vec<String>,
    ) {
        let now = system_time_ms(SystemTime::now());
        let status = self.statuses.get_mut(&file).expect("live control status");
        let recovered_from_failure = status.retained_error.is_some();
        status.last_applied_at_ms = Some(now);
        status.retained_error = None;
        status.recovery_source = Some(recovery_source.into());
        status.quarantined = quarantined;
        status.warnings = warnings;
        if recovered_from_failure || status.recovery_source.as_deref() == Some("last-known-good") {
            status.last_recovered_at_ms = Some(now);
        }
    }

    pub fn failed(&mut self, file: LiveControlFile, error: impl Into<String>) {
        self.statuses
            .get_mut(&file)
            .expect("live control status")
            .retained_error = Some(error.into());
    }
}

fn system_time_ms(time: SystemTime) -> u64 {
    time.duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum FileFingerprint {
    Missing,
    Bytes(blake3::Hash),
    Unreadable(io::ErrorKind, String),
}

fn fingerprint(path: &Path) -> FileFingerprint {
    match std::fs::read(path) {
        Ok(bytes) => FileFingerprint::Bytes(blake3::hash(&bytes)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => FileFingerprint::Missing,
        Err(error) => FileFingerprint::Unreadable(error.kind(), error.to_string()),
    }
}

#[derive(Debug)]
struct WatchSlot {
    observed: FileFingerprint,
    changed_at: Option<Instant>,
}

/// Content-based, rename-safe debounce state for the three installed live
/// controls. The managed owner supplies the clock and polling cadence; this
/// object only coalesces stable content transitions. Reading bytes rather than
/// mtimes catches same-tick atomic replacement and avoids editor-specific
/// filesystem event sequences.
#[derive(Debug)]
pub struct LiveControlWatcher {
    app_data: PathBuf,
    debounce: Duration,
    slots: BTreeMap<LiveControlFile, WatchSlot>,
}

impl LiveControlWatcher {
    pub fn new(app_data: PathBuf, debounce: Duration) -> Self {
        let slots = LiveControlFile::ALL
            .into_iter()
            .map(|file| {
                (
                    file,
                    WatchSlot {
                        observed: fingerprint(&file.path(&app_data)),
                        changed_at: None,
                    },
                )
            })
            .collect();
        Self {
            app_data,
            debounce,
            slots,
        }
    }

    pub fn poll(&mut self, now: Instant) -> Vec<LiveControlFile> {
        let mut ready = Vec::new();
        for file in LiveControlFile::ALL {
            let current = fingerprint(&file.path(&self.app_data));
            let slot = self.slots.get_mut(&file).expect("live control watch slot");
            if current != slot.observed {
                slot.observed = current;
                slot.changed_at = Some(now);
            } else if slot
                .changed_at
                .is_some_and(|changed_at| now.duration_since(changed_at) >= self.debounce)
            {
                slot.changed_at = None;
                ready.push(file);
            }
        }
        ready
    }

    /// A loader may quarantine/restore the pathname while applying a change.
    /// Adopt those committed bytes immediately so recovery cannot trigger a
    /// duplicate application one debounce window later.
    pub fn acknowledge(&mut self, file: LiveControlFile) {
        let slot = self.slots.get_mut(&file).expect("live control watch slot");
        slot.observed = fingerprint(&file.path(&self.app_data));
        slot.changed_at = None;
    }
}

fn settings_lkg_path(app_data: &Path) -> PathBuf {
    app_data.join("settings.json.lkg")
}

fn device_id_path(app_data: &Path) -> PathBuf {
    app_data.join("device-id")
}

fn device_id_lkg_path(app_data: &Path) -> PathBuf {
    app_data.join("device-id.lkg")
}

/// Explicit product recovery for an identity whose primary and LKG copies are
/// both unusable. This is intentionally separate from `device_id_checked`:
/// startup never silently changes replica identity, while the fatal recovery
/// button may mint one after the user confirms the reset. Existing quarantine
/// evidence is retained for diagnostics.
pub fn reset_device_identity(app_data: &Path) -> Result<String, ControlFileIssue> {
    use photoproof_core::id::DEVICE_ID_LEN;

    std::fs::create_dir_all(app_data)
        .map_err(|error| ControlFileIssue::from_io(app_data, error))?;
    let seed = format!("{}{}", ulid::Ulid::new(), ulid::Ulid::new());
    let id = blake3::hash(seed.as_bytes()).to_hex().to_string()[..DEVICE_ID_LEN].to_owned();
    let lkg_path = device_id_lkg_path(app_data);
    let path = device_id_path(app_data);
    // LKG first preserves the same interrupted-first-write recovery invariant
    // used by fresh installation.
    atomic_write(&lkg_path, id.as_bytes())
        .map_err(|error| ControlFileIssue::from_io(&lkg_path, error))?;
    atomic_write(&path, id.as_bytes()).map_err(|error| ControlFileIssue::from_io(&path, error))?;
    Ok(id)
}

/// Where a usable control-file value came from. This is deliberately a
/// backend wire type: startup health can expose recovery without parsing log
/// strings or guessing that a default value means the file was absent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum ControlFileSource {
    Primary,
    LastKnownGood,
    MissingDefault,
    Created,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum ControlFileFailureKind {
    Missing,
    Corrupt,
    PermissionDenied,
    Io,
}

/// Structured control-file failure. `quarantined_path` is populated only
/// after the invalid bytes have been durably moved out of the live pathname.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ControlFileIssue {
    pub kind: ControlFileFailureKind,
    pub path: PathBuf,
    pub detail: String,
    pub quarantined_path: Option<PathBuf>,
}

impl std::fmt::Display for ControlFileIssue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{:?} control file {}: {}",
            self.kind,
            self.path.display(),
            self.detail
        )
    }
}

impl std::error::Error for ControlFileIssue {}

impl ControlFileIssue {
    fn from_io(path: &Path, error: io::Error) -> Self {
        let kind = match error.kind() {
            io::ErrorKind::NotFound => ControlFileFailureKind::Missing,
            io::ErrorKind::PermissionDenied => ControlFileFailureKind::PermissionDenied,
            _ => ControlFileFailureKind::Io,
        };
        Self {
            kind,
            path: path.to_owned(),
            detail: error.to_string(),
            quarantined_path: None,
        }
    }

    fn corrupt(path: &Path, detail: impl Into<String>) -> Self {
        Self {
            kind: ControlFileFailureKind::Corrupt,
            path: path.to_owned(),
            detail: detail.into(),
            quarantined_path: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ControlFileRecovery {
    pub source: ControlFileSource,
    pub quarantined: Vec<PathBuf>,
    /// Non-fatal durability trouble, such as a readable primary whose LKG
    /// refresh could not be written. Callers should surface this as degraded
    /// health while continuing with the known-good primary value.
    pub warnings: Vec<ControlFileIssue>,
}

impl ControlFileRecovery {
    fn from_source(source: ControlFileSource) -> Self {
        Self {
            source,
            quarantined: Vec::new(),
            warnings: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SettingsLoad {
    pub settings: AppSettings,
    pub recovery: ControlFileRecovery,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DeviceIdentityLoad {
    pub device_id: String,
    pub recovery: ControlFileRecovery,
}

fn read_optional(path: &Path) -> Result<Option<Vec<u8>>, ControlFileIssue> {
    match std::fs::read(path) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(ControlFileIssue::from_io(path, error)),
    }
}

fn parse_settings(path: &Path) -> Result<Option<AppSettings>, ControlFileIssue> {
    let Some(bytes) = read_optional(path)? else {
        return Ok(None);
    };
    let settings: AppSettings = serde_json::from_slice(&bytes)
        .map_err(|error| ControlFileIssue::corrupt(path, error.to_string()))?;
    validate_settings(&settings).map_err(|detail| ControlFileIssue::corrupt(path, detail))?;
    Ok(Some(settings))
}

fn validate_settings(settings: &AppSettings) -> Result<(), String> {
    if settings.preview_cache_budget_bytes == 0 {
        return Err(
            "previewCacheBudgetBytes must be greater than zero; zero would evict every 1:1 preview"
                .into(),
        );
    }
    Ok(())
}

fn parse_device_id(path: &Path) -> Result<Option<String>, ControlFileIssue> {
    use photoproof_core::id::DEVICE_ID_LEN;

    let Some(bytes) = read_optional(path)? else {
        return Ok(None);
    };
    let text = std::str::from_utf8(&bytes)
        .map_err(|error| ControlFileIssue::corrupt(path, format!("invalid UTF-8: {error}")))?;
    let id = text.trim();
    if id.len() != DEVICE_ID_LEN
        || !id
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    {
        return Err(ControlFileIssue::corrupt(
            path,
            format!("expected {DEVICE_ID_LEN} lowercase hexadecimal characters"),
        ));
    }
    Ok(Some(id.to_owned()))
}

fn serialize_settings(settings: &AppSettings) -> io::Result<Vec<u8>> {
    validate_settings(settings)
        .map_err(|detail| io::Error::new(io::ErrorKind::InvalidInput, detail))?;
    serde_json::to_vec_pretty(settings)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
}

fn temp_path(path: &Path) -> PathBuf {
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("control-file");
    path.with_file_name(format!(".{name}.tmp-{}", ulid::Ulid::new()))
}

/// Write beside the destination, flush file contents, atomically replace the
/// live pathname, then durably flush the containing directory metadata.
fn atomic_write(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let parent = path.parent().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("control file has no parent: {}", path.display()),
        )
    })?;
    std::fs::create_dir_all(parent)?;

    let tmp = temp_path(path);
    let result = (|| {
        let mut file = OpenOptions::new().write(true).create_new(true).open(&tmp)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        drop(file);
        atomic_replace(&tmp, path)?;
        sync_parent(parent)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

#[cfg(not(target_os = "windows"))]
fn atomic_replace(from: &Path, to: &Path) -> io::Result<()> {
    std::fs::rename(from, to)
}

#[cfg(target_os = "windows")]
fn atomic_replace(from: &Path, to: &Path) -> io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::{
        MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH, MoveFileExW,
    };

    let from: Vec<u16> = from.as_os_str().encode_wide().chain(Some(0)).collect();
    let to: Vec<u16> = to.as_os_str().encode_wide().chain(Some(0)).collect();
    // MOVEFILE_WRITE_THROUGH covers both file and rename metadata durability
    // on Windows, whose stdlib cannot open a directory for `sync_all`.
    let ok = unsafe {
        MoveFileExW(
            from.as_ptr(),
            to.as_ptr(),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    };
    if ok == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

#[cfg(not(target_os = "windows"))]
fn sync_parent(parent: &Path) -> io::Result<()> {
    File::open(parent)?.sync_all()
}

#[cfg(target_os = "windows")]
fn sync_parent(_parent: &Path) -> io::Result<()> {
    // `atomic_replace` uses MOVEFILE_WRITE_THROUGH because directory handles
    // cannot be opened through portable std APIs on Windows.
    Ok(())
}

#[cfg(not(target_os = "windows"))]
fn durable_move_to_new(from: &Path, to: &Path) -> io::Result<()> {
    std::fs::rename(from, to)?;
    sync_parent(to.parent().unwrap_or_else(|| Path::new(".")))
}

#[cfg(target_os = "windows")]
fn durable_move_to_new(from: &Path, to: &Path) -> io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::{MOVEFILE_WRITE_THROUGH, MoveFileExW};

    let from: Vec<u16> = from.as_os_str().encode_wide().chain(Some(0)).collect();
    let to: Vec<u16> = to.as_os_str().encode_wide().chain(Some(0)).collect();
    let ok = unsafe { MoveFileExW(from.as_ptr(), to.as_ptr(), MOVEFILE_WRITE_THROUGH) };
    if ok == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

fn quarantine(path: &Path, mut issue: ControlFileIssue) -> Result<PathBuf, ControlFileIssue> {
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("control-file");
    let quarantine = path.with_file_name(format!("{name}.corrupt-{}", ulid::Ulid::new()));
    durable_move_to_new(path, &quarantine)
        .map_err(|error| ControlFileIssue::from_io(path, error))?;
    issue.quarantined_path = Some(quarantine.clone());
    Ok(quarantine)
}

fn settings_from_lkg(
    app_data: &Path,
    recovery: &mut ControlFileRecovery,
) -> Result<Option<AppSettings>, ControlFileIssue> {
    let lkg = settings_lkg_path(app_data);
    match parse_settings(&lkg) {
        Ok(settings) => Ok(settings),
        Err(issue) if issue.kind == ControlFileFailureKind::Corrupt => {
            let quarantined = quarantine(&lkg, issue.clone())?;
            recovery.quarantined.push(quarantined);
            Err(ControlFileIssue {
                quarantined_path: recovery.quarantined.last().cloned(),
                ..issue
            })
        }
        Err(issue) => Err(issue),
    }
}

/// Load settings with an explicit missing/corrupt/permission recovery result.
/// Missing settings intentionally use defaults. Invalid settings never do:
/// they are quarantined and must either recover from a valid LKG or return a
/// structured error to the caller.
pub fn load_checked(app_data: &Path) -> Result<SettingsLoad, ControlFileIssue> {
    let primary = settings_path(app_data);
    match parse_settings(&primary) {
        Ok(Some(settings)) => {
            let mut recovery = ControlFileRecovery::from_source(ControlFileSource::Primary);
            let lkg_path = settings_lkg_path(app_data);
            match parse_settings(&lkg_path) {
                Err(issue) if issue.kind == ControlFileFailureKind::Corrupt => {
                    match quarantine(&lkg_path, issue) {
                        Ok(quarantined) => recovery.quarantined.push(quarantined),
                        Err(issue) => recovery.warnings.push(issue),
                    }
                }
                Err(issue) => recovery.warnings.push(issue),
                Ok(_) => {}
            }
            match serialize_settings(&settings).and_then(|bytes| atomic_write(&lkg_path, &bytes)) {
                Ok(()) => {}
                Err(error) => recovery
                    .warnings
                    .push(ControlFileIssue::from_io(&lkg_path, error)),
            }
            Ok(SettingsLoad { settings, recovery })
        }
        Ok(None) => {
            let mut recovery = ControlFileRecovery::from_source(ControlFileSource::MissingDefault);
            if let Some(settings) = settings_from_lkg(app_data, &mut recovery)? {
                let bytes = serialize_settings(&settings)
                    .map_err(|error| ControlFileIssue::from_io(&primary, error))?;
                atomic_write(&primary, &bytes)
                    .map_err(|error| ControlFileIssue::from_io(&primary, error))?;
                recovery.source = ControlFileSource::LastKnownGood;
                return Ok(SettingsLoad { settings, recovery });
            }
            if quarantine_exists(app_data, "settings.json.corrupt-")? {
                return Err(ControlFileIssue {
                    kind: ControlFileFailureKind::Missing,
                    path: primary,
                    detail: "settings are missing after a prior corruption quarantine and no last-known-good copy exists".into(),
                    quarantined_path: None,
                });
            }
            Ok(SettingsLoad {
                settings: AppSettings::default(),
                recovery,
            })
        }
        Err(issue) if issue.kind == ControlFileFailureKind::Corrupt => {
            let quarantined = quarantine(&primary, issue.clone())?;
            let mut recovery = ControlFileRecovery::from_source(ControlFileSource::LastKnownGood);
            recovery.quarantined.push(quarantined.clone());
            match settings_from_lkg(app_data, &mut recovery) {
                Ok(Some(settings)) => {
                    let bytes = serialize_settings(&settings)
                        .map_err(|error| ControlFileIssue::from_io(&primary, error))?;
                    atomic_write(&primary, &bytes)
                        .map_err(|error| ControlFileIssue::from_io(&primary, error))?;
                    Ok(SettingsLoad { settings, recovery })
                }
                Ok(None) => Err(ControlFileIssue {
                    quarantined_path: Some(quarantined),
                    ..issue
                }),
                Err(lkg_issue) => Err(ControlFileIssue {
                    detail: format!(
                        "{}; last-known-good recovery failed at {}: {}",
                        issue.detail,
                        lkg_issue.path.display(),
                        lkg_issue.detail
                    ),
                    quarantined_path: Some(quarantined),
                    ..issue
                }),
            }
        }
        Err(issue) => Err(issue),
    }
}

pub fn save(app_data: &Path, s: &AppSettings) -> std::io::Result<()> {
    let bytes = serialize_settings(s)?;
    let path = settings_path(app_data);
    // LKG first: a successful primary commit always has a complete same-value
    // fallback. If we crash between the two writes, the primary remains the
    // authoritative previously committed value.
    atomic_write(&settings_lkg_path(app_data), &bytes)?;
    atomic_write(&path, &bytes)
}

/// Explicit Restore defaults is a real durable commit, not deletion. Writing
/// both primary and LKG prevents the ordinary missing-file recovery path from
/// resurrecting the user's previous settings on the next poll or launch.
pub fn restore_settings_defaults(app_data: &Path) -> io::Result<AppSettings> {
    let defaults = AppSettings::default();
    save(app_data, &defaults)?;
    Ok(defaults)
}

/// Config and tuning both use empty TOML as their canonical all-defaults
/// representation. Commit through the shared control-file primitive so Restore
/// defaults replaces primary and LKG atomically in the same ordering as every
/// other durable control mutation.
pub fn restore_toml_defaults(app_data: &Path, file: LiveControlFile) -> io::Result<()> {
    let path = match file {
        LiveControlFile::Config | LiveControlFile::Tuning => file.path(app_data),
        LiveControlFile::Settings => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "settings defaults use restore_settings_defaults",
            ));
        }
    };
    photoproof_core::runtime::save_control(&path, b"")
}

/// Bound retained corrupt-file evidence per installed control. The newest
/// ULID-suffixed artifacts remain available for field drills and diagnostics;
/// older artifacts are rebuildable evidence and are removed after a newer
/// sample exists. Failures are returned so health can retain them.
pub fn prune_control_file_quarantines(app_data: &Path, keep: usize) -> io::Result<Vec<PathBuf>> {
    let mut removed = Vec::new();
    for file in LiveControlFile::ALL {
        let prefix = format!(
            "{}.corrupt-",
            file.path(app_data)
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or(file.name())
        );
        let mut paths = std::fs::read_dir(app_data)?
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|path| {
                path.file_name()
                    .is_some_and(|name| name.to_string_lossy().starts_with(&prefix))
            })
            .collect::<Vec<_>>();
        paths.sort();
        let remove_count = paths.len().saturating_sub(keep);
        for path in paths.into_iter().take(remove_count) {
            std::fs::remove_file(&path)?;
            removed.push(path);
        }
    }
    Ok(removed)
}

/// Random-per-install device id: 32 lowercase hex (EVENTS §9), persisted in
/// app data. The length check and the mint-time truncation below MUST use
/// the same core constant: if they disagreed, freshly minted ids would
/// fail this very validation on the next launch. Invalid persisted identities
/// are quarantined and recovered from LKG (or fail closed), never re-minted.
pub fn device_id_checked(app_data: &Path) -> Result<DeviceIdentityLoad, ControlFileIssue> {
    use photoproof_core::id::DEVICE_ID_LEN;

    let path = device_id_path(app_data);
    let lkg_path = device_id_lkg_path(app_data);
    match parse_device_id(&path) {
        Ok(Some(device_id)) => {
            let mut recovery = ControlFileRecovery::from_source(ControlFileSource::Primary);
            match parse_device_id(&lkg_path) {
                Ok(Some(lkg)) if lkg == device_id => {}
                Ok(Some(_))
                | Err(ControlFileIssue {
                    kind: ControlFileFailureKind::Corrupt,
                    ..
                }) => {
                    match quarantine(
                        &lkg_path,
                        ControlFileIssue::corrupt(
                            &lkg_path,
                            "last-known-good identity disagreed with primary",
                        ),
                    ) {
                        Ok(quarantined) => recovery.quarantined.push(quarantined),
                        Err(issue) => recovery.warnings.push(issue),
                    }
                    if let Err(error) = atomic_write(&lkg_path, device_id.as_bytes()) {
                        recovery
                            .warnings
                            .push(ControlFileIssue::from_io(&lkg_path, error));
                    }
                }
                Ok(None) => {
                    if let Err(error) = atomic_write(&lkg_path, device_id.as_bytes()) {
                        recovery
                            .warnings
                            .push(ControlFileIssue::from_io(&lkg_path, error));
                    }
                }
                Err(issue) => recovery.warnings.push(issue),
            }
            Ok(DeviceIdentityLoad {
                device_id,
                recovery,
            })
        }
        Ok(None) => match parse_device_id(&lkg_path) {
            Ok(Some(device_id)) => {
                atomic_write(&path, device_id.as_bytes())
                    .map_err(|error| ControlFileIssue::from_io(&path, error))?;
                Ok(DeviceIdentityLoad {
                    device_id,
                    recovery: ControlFileRecovery::from_source(ControlFileSource::LastKnownGood),
                })
            }
            Ok(None) => {
                if quarantine_exists(app_data, "device-id.corrupt-")? {
                    return Err(ControlFileIssue {
                        kind: ControlFileFailureKind::Missing,
                        path,
                        detail: "identity is missing after a prior corruption quarantine; refusing to mint a new replica identity".into(),
                        quarantined_path: None,
                    });
                }
                // Two fresh ULIDs hashed: 256 bits of randomness reduced to 32
                // hex. Persist LKG first so an interruption before the primary
                // rename recovers the same identity instead of minting again.
                let seed = format!("{}{}", ulid::Ulid::new(), ulid::Ulid::new());
                let id =
                    blake3::hash(seed.as_bytes()).to_hex().to_string()[..DEVICE_ID_LEN].to_owned();
                atomic_write(&lkg_path, id.as_bytes())
                    .map_err(|error| ControlFileIssue::from_io(&lkg_path, error))?;
                atomic_write(&path, id.as_bytes())
                    .map_err(|error| ControlFileIssue::from_io(&path, error))?;
                Ok(DeviceIdentityLoad {
                    device_id: id,
                    recovery: ControlFileRecovery::from_source(ControlFileSource::Created),
                })
            }
            Err(issue) if issue.kind == ControlFileFailureKind::Corrupt => {
                let quarantined = quarantine(&lkg_path, issue.clone())?;
                Err(ControlFileIssue {
                    quarantined_path: Some(quarantined),
                    ..issue
                })
            }
            Err(issue) => Err(issue),
        },
        Err(issue) if issue.kind == ControlFileFailureKind::Corrupt => {
            let quarantined = quarantine(&path, issue.clone())?;
            match parse_device_id(&lkg_path) {
                Ok(Some(device_id)) => {
                    atomic_write(&path, device_id.as_bytes())
                        .map_err(|error| ControlFileIssue::from_io(&path, error))?;
                    let mut recovery =
                        ControlFileRecovery::from_source(ControlFileSource::LastKnownGood);
                    recovery.quarantined.push(quarantined);
                    Ok(DeviceIdentityLoad {
                        device_id,
                        recovery,
                    })
                }
                Ok(None) => Err(ControlFileIssue {
                    quarantined_path: Some(quarantined),
                    ..issue
                }),
                Err(lkg_issue) if lkg_issue.kind == ControlFileFailureKind::Corrupt => {
                    let lkg_quarantine = quarantine(&lkg_path, lkg_issue)?;
                    Err(ControlFileIssue {
                        detail: format!(
                            "{}; last-known-good identity was also corrupt ({})",
                            issue.detail,
                            lkg_quarantine.display()
                        ),
                        quarantined_path: Some(quarantined),
                        ..issue
                    })
                }
                Err(lkg_issue) => Err(lkg_issue),
            }
        }
        Err(issue) => Err(issue),
    }
}

fn quarantine_exists(app_data: &Path, prefix: &str) -> Result<bool, ControlFileIssue> {
    let entries =
        std::fs::read_dir(app_data).map_err(|error| ControlFileIssue::from_io(app_data, error))?;
    for entry in entries {
        let entry = entry.map_err(|error| ControlFileIssue::from_io(app_data, error))?;
        if entry.file_name().to_string_lossy().starts_with(prefix) {
            return Ok(true);
        }
    }
    Ok(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(
            load_checked(dir.path()).unwrap().settings.last_export_ts,
            None
        );
        // A fresh load carries the 20 GB budget default, not 0.
        assert_eq!(
            load_checked(dir.path())
                .unwrap()
                .settings
                .preview_cache_budget_bytes,
            DEFAULT_PREVIEW_CACHE_BUDGET_BYTES
        );
        let s = AppSettings {
            last_export_ts: Some("2026-06-09T12:00:00Z".into()),
            stack_display: StackDisplay::Raw,
            external_editor: Some("Affinity Photo".into()),
            preview_cache_budget_bytes: 50 * 1024 * 1024 * 1024,
            processing_intensity: ProcessingIntensity::Max,
            processing_paused: true,
            new_root_policy: NewRootPolicy::ProcessLater,
            root_processing_policies: BTreeMap::from([(
                "root-preview".into(),
                NewRootPolicy::PreviewOnly,
            )]),
            defer_text_embeddings: true,
            defer_image_embeddings: false,
        };
        save(dir.path(), &s).unwrap();
        let loaded = load_checked(dir.path()).unwrap().settings;
        assert_eq!(loaded.last_export_ts, s.last_export_ts);
        assert_eq!(loaded.stack_display, StackDisplay::Raw);
        assert_eq!(loaded.external_editor.as_deref(), Some("Affinity Photo"));
        // The edited budget round-trips through settings.json.
        assert_eq!(loaded.preview_cache_budget_bytes, 50 * 1024 * 1024 * 1024);
        assert_eq!(loaded.root_processing_policies, s.root_processing_policies);
        assert!(loaded.defer_text_embeddings);
        assert!(!loaded.defer_image_embeddings);
    }

    #[test]
    fn external_editor_defaults_to_none_for_pre_existing_files() {
        // A settings.json written before this field existed has no
        // externalEditor key; #[serde(default)] must load it as None (the
        // OS-default fallback) rather than fail the whole parse.
        let legacy = r#"{ "stackDisplay": "raw" }"#;
        let s: AppSettings = serde_json::from_str(legacy).unwrap();
        assert_eq!(s.external_editor, None);
        assert_eq!(s.stack_display, StackDisplay::Raw);
        assert_eq!(s.processing_intensity, ProcessingIntensity::Balanced);
        assert!(!s.processing_paused);
        assert_eq!(s.new_root_policy, NewRootPolicy::ProcessNow);
    }

    #[test]
    fn preview_cache_budget_defaults_to_20gb_for_pre_existing_files() {
        // A settings.json written before this field existed has no
        // previewCacheBudgetBytes key; the custom serde default must load it as
        // 20 GB, NOT 0 (a 0 budget would evict the whole 1:1 cache on the first
        // develop).
        let legacy = r#"{ "stackDisplay": "raw" }"#;
        let s: AppSettings = serde_json::from_str(legacy).unwrap();
        assert_eq!(
            s.preview_cache_budget_bytes,
            DEFAULT_PREVIEW_CACHE_BUDGET_BYTES
        );
        assert_eq!(DEFAULT_PREVIEW_CACHE_BUDGET_BYTES, 20 * 1024 * 1024 * 1024);
    }

    #[test]
    fn stack_display_defaults_to_jpeg_and_speaks_lowercase_json() {
        // Pre-existing settings.json files (no stackDisplay key) load with
        // the JPEG default; the wire form matches the TS union "jpeg"|"raw".
        let s: AppSettings = serde_json::from_str("{}").unwrap();
        assert_eq!(s.stack_display, StackDisplay::Jpeg);
        let json = serde_json::to_string(&AppSettings {
            last_export_ts: None,
            stack_display: StackDisplay::Raw,
            external_editor: None,
            preview_cache_budget_bytes: DEFAULT_PREVIEW_CACHE_BUDGET_BYTES,
            processing_intensity: ProcessingIntensity::Balanced,
            processing_paused: false,
            new_root_policy: NewRootPolicy::ProcessNow,
            root_processing_policies: BTreeMap::new(),
            defer_text_embeddings: false,
            defer_image_embeddings: false,
        })
        .unwrap();
        assert!(json.contains("\"stackDisplay\":\"raw\""), "got: {json}");
        // The budget serializes camelCase too (the UI reads it back).
        assert!(json.contains("\"previewCacheBudgetBytes\":"), "got: {json}");
    }

    #[test]
    fn device_id_is_stable_32_lowercase_hex() {
        let dir = tempfile::tempdir().unwrap();
        let a = device_id_checked(dir.path()).unwrap().device_id;
        let b = device_id_checked(dir.path()).unwrap().device_id;
        assert_eq!(a, b);
        assert_eq!(a.len(), 32);
        assert!(
            a.bytes()
                .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
        );
        photoproof_core::id::validate_device_id(&a).expect("valid per EVENTS §9");
    }

    #[test]
    fn missing_settings_are_distinct_from_corruption() {
        let dir = tempfile::tempdir().unwrap();
        let loaded = load_checked(dir.path()).unwrap();
        assert_eq!(loaded.recovery.source, ControlFileSource::MissingDefault);
        assert_eq!(
            loaded.settings.preview_cache_budget_bytes,
            DEFAULT_PREVIEW_CACHE_BUDGET_BYTES
        );
        assert!(loaded.recovery.quarantined.is_empty());
    }

    #[test]
    fn corrupt_settings_are_quarantined_and_recovered_from_lkg() {
        let dir = tempfile::tempdir().unwrap();
        let expected = AppSettings {
            last_export_ts: Some("2026-07-26T12:00:00Z".into()),
            stack_display: StackDisplay::Raw,
            external_editor: Some("Darktable".into()),
            preview_cache_budget_bytes: 42,
            processing_intensity: ProcessingIntensity::Eco,
            processing_paused: true,
            new_root_policy: NewRootPolicy::ProcessLater,
            root_processing_policies: BTreeMap::new(),
            defer_text_embeddings: true,
            defer_image_embeddings: true,
        };
        save(dir.path(), &expected).unwrap();
        std::fs::write(settings_path(dir.path()), b"{\"stackDisplay\":").unwrap();

        let loaded = load_checked(dir.path()).unwrap();
        assert_eq!(loaded.recovery.source, ControlFileSource::LastKnownGood);
        assert_eq!(loaded.settings.last_export_ts, expected.last_export_ts);
        assert_eq!(loaded.settings.stack_display, StackDisplay::Raw);
        assert_eq!(loaded.recovery.quarantined.len(), 1);
        assert!(loaded.recovery.quarantined[0].exists());
        assert!(settings_path(dir.path()).exists());
    }

    #[test]
    fn corrupt_settings_lkg_is_quarantined_without_displacing_valid_primary() {
        let dir = tempfile::tempdir().unwrap();
        let expected = AppSettings {
            last_export_ts: Some("primary-wins".into()),
            ..AppSettings::default()
        };
        save(dir.path(), &expected).unwrap();
        std::fs::write(settings_lkg_path(dir.path()), b"{").unwrap();

        let loaded = load_checked(dir.path()).unwrap();
        assert_eq!(loaded.recovery.source, ControlFileSource::Primary);
        assert_eq!(
            loaded.settings.last_export_ts.as_deref(),
            Some("primary-wins")
        );
        assert_eq!(loaded.recovery.quarantined.len(), 1);
        assert!(loaded.recovery.quarantined[0].exists());
        parse_settings(&settings_lkg_path(dir.path()))
            .unwrap()
            .expect("LKG rewritten from valid primary");
    }

    #[test]
    fn corrupt_settings_without_lkg_never_turn_into_silent_missing_defaults() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(settings_path(dir.path()), b"{").unwrap();

        let first = load_checked(dir.path()).unwrap_err();
        assert_eq!(first.kind, ControlFileFailureKind::Corrupt);
        let quarantine = first.quarantined_path.expect("quarantined invalid bytes");
        assert!(quarantine.exists());
        assert!(!settings_path(dir.path()).exists());

        let second = load_checked(dir.path()).unwrap_err();
        assert_eq!(second.kind, ControlFileFailureKind::Missing);
        assert!(second.detail.contains("prior corruption quarantine"));
    }

    #[test]
    fn interrupted_settings_temp_does_not_replace_committed_primary() {
        let dir = tempfile::tempdir().unwrap();
        let expected = AppSettings {
            last_export_ts: Some("committed".into()),
            ..AppSettings::default()
        };
        save(dir.path(), &expected).unwrap();
        std::fs::write(dir.path().join(".settings.json.tmp-interrupted"), b"{").unwrap();

        let loaded = load_checked(dir.path()).unwrap();
        assert_eq!(loaded.recovery.source, ControlFileSource::Primary);
        assert_eq!(loaded.settings.last_export_ts.as_deref(), Some("committed"));
    }

    #[test]
    fn corrupt_device_id_recovers_exact_identity_from_lkg() {
        let dir = tempfile::tempdir().unwrap();
        let original = device_id_checked(dir.path()).unwrap().device_id;
        std::fs::write(device_id_path(dir.path()), b"truncated").unwrap();

        let loaded = device_id_checked(dir.path()).unwrap();
        assert_eq!(loaded.device_id, original);
        assert_eq!(loaded.recovery.source, ControlFileSource::LastKnownGood);
        assert_eq!(loaded.recovery.quarantined.len(), 1);
        assert_eq!(
            std::fs::read_to_string(device_id_path(dir.path())).unwrap(),
            original
        );
    }

    #[test]
    fn corrupt_device_id_without_lkg_refuses_to_mint_forever() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(device_id_path(dir.path()), b"not-an-identity").unwrap();

        let first = device_id_checked(dir.path()).unwrap_err();
        assert_eq!(first.kind, ControlFileFailureKind::Corrupt);
        let quarantined = first.quarantined_path.expect("invalid id quarantined");
        assert!(quarantined.exists());
        assert!(!device_id_path(dir.path()).exists());
        assert!(!device_id_lkg_path(dir.path()).exists());

        let second = device_id_checked(dir.path()).unwrap_err();
        assert_eq!(second.kind, ControlFileFailureKind::Missing);
        assert!(second.detail.contains("refusing to mint"));
        assert!(!device_id_path(dir.path()).exists());
    }

    #[test]
    fn explicit_device_identity_reset_recovers_the_fail_closed_startup() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(device_id_path(dir.path()), b"not-an-identity").unwrap();
        let first = device_id_checked(dir.path()).unwrap_err();
        let quarantine = first.quarantined_path.expect("quarantine");

        let reset = reset_device_identity(dir.path()).expect("explicit reset");
        let loaded = device_id_checked(dir.path()).expect("next startup");
        assert_eq!(loaded.device_id, reset);
        assert_eq!(loaded.recovery.source, ControlFileSource::Primary);
        assert!(quarantine.exists(), "diagnostic quarantine is retained");
    }

    #[test]
    fn missing_primary_device_id_recovers_interrupted_first_write_from_lkg() {
        let dir = tempfile::tempdir().unwrap();
        let id = "0123456789abcdef0123456789abcdef";
        std::fs::write(device_id_lkg_path(dir.path()), id).unwrap();

        let loaded = device_id_checked(dir.path()).unwrap();
        assert_eq!(loaded.device_id, id);
        assert_eq!(loaded.recovery.source, ControlFileSource::LastKnownGood);
        assert_eq!(
            std::fs::read_to_string(device_id_path(dir.path())).unwrap(),
            id
        );
    }

    #[cfg(unix)]
    #[test]
    fn settings_permission_error_is_not_reported_as_missing_or_corrupt() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let path = settings_path(dir.path());
        std::fs::write(&path, b"{}").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o000)).unwrap();

        let issue = load_checked(dir.path()).unwrap_err();
        assert_eq!(issue.kind, ControlFileFailureKind::PermissionDenied);
        assert_eq!(issue.path, path);
    }

    #[cfg(unix)]
    #[test]
    fn device_id_permission_error_never_mints_or_quarantines() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let path = device_id_path(dir.path());
        let original = "0123456789abcdef0123456789abcdef";
        std::fs::write(&path, original).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o000)).unwrap();

        let issue = device_id_checked(dir.path()).unwrap_err();
        assert_eq!(issue.kind, ControlFileFailureKind::PermissionDenied);
        assert_eq!(issue.path, path);
        assert!(!device_id_lkg_path(dir.path()).exists());
        let quarantines = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(Result::ok)
            .filter(|entry| {
                entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with("device-id.corrupt-")
            })
            .count();
        assert_eq!(quarantines, 0);
    }

    #[test]
    fn watcher_detects_atomic_replacement_after_one_stable_debounce() {
        let dir = tempfile::tempdir().unwrap();
        let start = Instant::now();
        let mut watcher = LiveControlWatcher::new(dir.path().to_owned(), Duration::from_millis(50));
        let temp = dir.path().join(".config.toml.editor");
        std::fs::write(&temp, "[runtime]\ntier = 1\n").unwrap();
        std::fs::rename(&temp, dir.path().join("config.toml")).unwrap();

        assert!(watcher.poll(start).is_empty());
        assert!(watcher.poll(start + Duration::from_millis(49)).is_empty());
        assert_eq!(
            watcher.poll(start + Duration::from_millis(50)),
            vec![LiveControlFile::Config]
        );
        assert!(watcher.poll(start + Duration::from_millis(100)).is_empty());
    }

    #[test]
    fn watcher_coalesces_rapid_changes_to_the_last_stable_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let path = settings_path(dir.path());
        let start = Instant::now();
        let mut watcher = LiveControlWatcher::new(dir.path().to_owned(), Duration::from_millis(50));

        for (offset, display) in [(0, "raw"), (20, "jpeg"), (40, "raw")] {
            std::fs::write(&path, format!(r#"{{"stackDisplay":"{display}"}}"#)).unwrap();
            assert!(
                watcher
                    .poll(start + Duration::from_millis(offset))
                    .is_empty()
            );
        }
        assert!(watcher.poll(start + Duration::from_millis(89)).is_empty());
        assert_eq!(
            watcher.poll(start + Duration::from_millis(90)),
            vec![LiveControlFile::Settings]
        );
        assert_eq!(
            load_checked(dir.path()).unwrap().settings.stack_display,
            StackDisplay::Raw
        );
    }

    #[test]
    fn watcher_reports_deletion_and_acknowledges_loader_recovery_once() {
        let dir = tempfile::tempdir().unwrap();
        let committed = AppSettings {
            stack_display: StackDisplay::Raw,
            ..AppSettings::default()
        };
        save(dir.path(), &committed).unwrap();
        let start = Instant::now();
        let mut watcher = LiveControlWatcher::new(dir.path().to_owned(), Duration::from_millis(25));
        std::fs::remove_file(settings_path(dir.path())).unwrap();

        assert!(watcher.poll(start).is_empty());
        assert_eq!(
            watcher.poll(start + Duration::from_millis(25)),
            vec![LiveControlFile::Settings]
        );
        let recovered = load_checked(dir.path()).unwrap();
        assert_eq!(recovered.settings, committed);
        assert_eq!(recovered.recovery.source, ControlFileSource::LastKnownGood);
        watcher.acknowledge(LiveControlFile::Settings);
        assert!(watcher.poll(start + Duration::from_millis(100)).is_empty());
        assert!(watcher.poll(start + Duration::from_millis(200)).is_empty());
    }

    #[test]
    fn restore_defaults_replaces_primary_and_lkg_instead_of_resurrecting_old_values() {
        let dir = tempfile::tempdir().unwrap();
        save(
            dir.path(),
            &AppSettings {
                stack_display: StackDisplay::Raw,
                processing_paused: true,
                ..AppSettings::default()
            },
        )
        .unwrap();

        let restored = restore_settings_defaults(dir.path()).unwrap();
        assert_eq!(restored, AppSettings::default());
        std::fs::remove_file(settings_path(dir.path())).unwrap();
        let recovered = load_checked(dir.path()).unwrap();
        assert_eq!(recovered.settings, AppSettings::default());
        assert_eq!(recovered.recovery.source, ControlFileSource::LastKnownGood);

        restore_toml_defaults(dir.path(), LiveControlFile::Config).unwrap();
        restore_toml_defaults(dir.path(), LiveControlFile::Tuning).unwrap();
        assert_eq!(std::fs::read(dir.path().join("config.toml")).unwrap(), b"");
        assert_eq!(
            std::fs::read(dir.path().join("config.toml.lkg")).unwrap(),
            b""
        );
        assert_eq!(std::fs::read(dir.path().join("tuning.toml")).unwrap(), b"");
        assert_eq!(
            std::fs::read(dir.path().join("tuning.toml.lkg")).unwrap(),
            b""
        );
    }

    #[test]
    fn semantically_invalid_settings_recover_lkg_instead_of_applying_dangerous_zero_budget() {
        let dir = tempfile::tempdir().unwrap();
        let committed = AppSettings {
            preview_cache_budget_bytes: 9_000_000,
            ..AppSettings::default()
        };
        save(dir.path(), &committed).unwrap();
        std::fs::write(
            settings_path(dir.path()),
            br#"{"previewCacheBudgetBytes":0}"#,
        )
        .unwrap();

        let loaded = load_checked(dir.path()).unwrap();
        assert_eq!(loaded.settings, committed);
        assert_eq!(loaded.recovery.source, ControlFileSource::LastKnownGood);
        assert_eq!(loaded.recovery.quarantined.len(), 1);
    }

    #[test]
    fn quarantine_retention_keeps_the_newest_installed_drill_evidence() {
        let dir = tempfile::tempdir().unwrap();
        for suffix in ["01", "02", "03", "04"] {
            std::fs::write(
                dir.path().join(format!("config.toml.corrupt-{suffix}")),
                suffix,
            )
            .unwrap();
        }
        let removed = prune_control_file_quarantines(dir.path(), 2).unwrap();
        assert_eq!(removed.len(), 2);
        assert!(!dir.path().join("config.toml.corrupt-01").exists());
        assert!(!dir.path().join("config.toml.corrupt-02").exists());
        assert!(dir.path().join("config.toml.corrupt-03").exists());
        assert!(dir.path().join("config.toml.corrupt-04").exists());
    }
}
