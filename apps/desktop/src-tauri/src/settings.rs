//! Minimal app-side settings persistence. UI preferences (sort per folder,
//! thumb size, rail pin) live in webview localStorage; state the Rust side
//! owns lands here: the last-export timestamp shown inline in Settings →
//! Export (spec/UI.md §2.4) and the stacked-pair display preference —
//! edited in the Settings window, consumed live by the main window, so it
//! needs a store both webviews share.

use std::path::{Path, PathBuf};

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

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
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
/// app data. The length check and the mint-time truncation below MUST use
/// the same core constant: if they disagreed, freshly minted ids would
/// fail this very validation on the next launch and silently re-mint
/// every run.
pub fn device_id(app_data: &Path) -> std::io::Result<String> {
    use photoproof_core::id::DEVICE_ID_LEN;
    let path = app_data.join("device-id");
    if let Ok(s) = std::fs::read_to_string(&path) {
        let s = s.trim().to_owned();
        if s.len() == DEVICE_ID_LEN
            && s.bytes()
                .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
        {
            return Ok(s);
        }
    }
    // Two fresh ULIDs hashed: 256 bits of randomness reduced to 32 hex.
    let seed = format!("{}{}", ulid::Ulid::new(), ulid::Ulid::new());
    let id = blake3::hash(seed.as_bytes()).to_hex().to_string()[..DEVICE_ID_LEN].to_owned();
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
            stack_display: StackDisplay::Raw,
            external_editor: Some("Affinity Photo".into()),
        };
        save(dir.path(), &s).unwrap();
        let loaded = load(dir.path());
        assert_eq!(loaded.last_export_ts, s.last_export_ts);
        assert_eq!(loaded.stack_display, StackDisplay::Raw);
        assert_eq!(loaded.external_editor.as_deref(), Some("Affinity Photo"));
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
        })
        .unwrap();
        assert!(json.contains("\"stackDisplay\":\"raw\""), "got: {json}");
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
