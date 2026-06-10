//! Minimal app-side settings persistence. UI preferences (sort per folder,
//! thumb size, rail pin) live in webview localStorage; only state the Rust
//! side owns lands here (currently: the last-export timestamp shown inline
//! in Settings → Export, spec/UI.md §2.4).

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct AppSettings {
    pub last_export_ts: Option<String>,
}

pub fn settings_path(app_data: &Path) -> PathBuf {
    app_data.join("settings.json")
}

pub fn load(app_data: &Path) -> AppSettings {
    std::fs::read(settings_path(app_data))
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

pub fn save(app_data: &Path, s: &AppSettings) -> std::io::Result<()> {
    let bytes = serde_json::to_vec_pretty(s).expect("settings serialize");
    let path = settings_path(app_data);
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(&tmp, &path)
}

/// Random-per-install device id: 32 lowercase hex (EVENTS §9), persisted in
/// app data.
pub fn device_id(app_data: &Path) -> std::io::Result<String> {
    let path = app_data.join("device-id");
    if let Ok(s) = std::fs::read_to_string(&path) {
        let s = s.trim().to_owned();
        if s.len() == 32
            && s.bytes()
                .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
        {
            return Ok(s);
        }
    }
    // Two fresh ULIDs hashed: 256 bits of randomness reduced to 32 hex.
    let seed = format!("{}{}", ulid::Ulid::new(), ulid::Ulid::new());
    let id = blake3::hash(seed.as_bytes()).to_hex().to_string()[..32].to_owned();
    std::fs::write(&path, &id)?;
    Ok(id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(load(dir.path()).last_export_ts, None);
        let s = AppSettings {
            last_export_ts: Some("2026-06-09T12:00:00Z".into()),
        };
        save(dir.path(), &s).unwrap();
        assert_eq!(load(dir.path()).last_export_ts, s.last_export_ts);
    }

    #[test]
    fn device_id_is_stable_32_lowercase_hex() {
        let dir = tempfile::tempdir().unwrap();
        let a = device_id(dir.path()).unwrap();
        let b = device_id(dir.path()).unwrap();
        assert_eq!(a, b);
        assert_eq!(a.len(), 32);
        assert!(
            a.bytes()
                .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
        );
        photoproof_core::id::validate_device_id(&a).expect("valid per EVENTS §9");
    }
}
