//! Preferences persistence (PLAN-GUI §11.3).
//!
//! Persisted to `<config>/kb-gui/prefs.toml` (via `dirs`). All fields are
//! `#[serde(default)]` so a partial / older file loads cleanly and a corrupt
//! file falls back to defaults (a bad prefs file must never prevent launch).

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DefaultEditorView {
    Formatted,
    #[default]
    Raw,
}

/// The user's persisted preferences (PLAN-GUI §11.3 schema).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct Preferences {
    /// Theme name (a built-in name or a custom TOML theme stem). Defaults to
    /// `marknote-dark`. An unknown name falls back to the nearest built-in.
    pub theme_name: String,
    pub default_editor_view: DefaultEditorView,
    pub autosave_delay_ms: u64,
    /// Privacy mode: redacts page titles in the tab/overview (G6 will wire
    /// the UI; the field persists now so a toggle doesn't lose its value).
    pub privacy_mode: bool,
    /// Restore the last-open page on launch (the G6 "Restore last page" item).
    pub restore_last_page: bool,
    /// The last-open page abs_path (only meaningful when `restore_last_page`).
    pub last_page: Option<String>,
}

impl Default for Preferences {
    fn default() -> Self {
        Self {
            theme_name: "marknote-dark".to_string(),
            default_editor_view: DefaultEditorView::Raw,
            autosave_delay_ms: 1200,
            privacy_mode: false,
            restore_last_page: true,
            last_page: None,
        }
    }
}

/// The `<config>/kb-gui` directory (created on demand by `save`).
pub fn config_dir() -> PathBuf {
    let base = dirs::config_dir().unwrap_or_else(|| PathBuf::from("."));
    base.join("kb-gui")
}

/// `<config>/kb-gui/prefs.toml`.
pub fn path() -> PathBuf {
    config_dir().join("prefs.toml")
}

/// Load preferences from disk. Missing → [`Preferences::default`]; corrupt →
/// defaults (logged via the returned `Ok` value; a bad file never blocks
/// launch). The `Ok` carries the loaded prefs *or* a fresh default so the
/// caller can tell whether a write-back on exit is warranted.
pub fn load() -> Preferences {
    let p = path();
    let Ok(s) = std::fs::read_to_string(&p) else {
        return Preferences::default();
    };
    toml::from_str::<Preferences>(&s).unwrap_or_default()
}

/// Save preferences to disk (creating `<config>/kb-gui` as needed). Failures
/// are non-fatal (a write error must not block the app); the return is
/// `Ok(())` only on a successful flush to disk.
pub fn save(prefs: &Preferences) -> Result<(), std::io::Error> {
    let dir = config_dir();
    std::fs::create_dir_all(&dir)?;
    let s = toml::to_string_pretty(prefs).map_err(std::io::Error::other)?;
    std::fs::write(path(), s)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_sane() {
        let p = Preferences::default();
        assert_eq!(p.theme_name, "marknote-dark");
        assert_eq!(p.default_editor_view, DefaultEditorView::Raw);
        assert_eq!(p.autosave_delay_ms, 1200);
        assert!(!p.privacy_mode);
        assert!(p.restore_last_page);
        assert!(p.last_page.is_none());
    }

    #[test]
    fn toml_roundtrip_via_strings() {
        // Pure parse/serialize round-trip (no fs) — locks the schema shape.
        let p = Preferences {
            theme_name: "high-contrast".to_string(),
            default_editor_view: DefaultEditorView::Formatted,
            autosave_delay_ms: 999,
            privacy_mode: true,
            restore_last_page: true,
            last_page: Some("/notes/two/three.md".to_string()),
        };
        let s = toml::to_string_pretty(&p).expect("serialize");
        let back: Preferences = toml::from_str(&s).expect("deserialize");
        assert_eq!(back, p);
    }

    #[test]
    fn partial_toml_fills_defaults() {
        let p: Preferences = toml::from_str(r#"theme_name = "marknote-light""#).expect("parse");
        assert_eq!(p.theme_name, "marknote-light");
        // Everything else falls back to defaults.
        assert_eq!(p.default_editor_view, DefaultEditorView::Raw);
        assert_eq!(p.autosave_delay_ms, 1200);
        assert!(p.restore_last_page);
    }

    #[test]
    fn unknown_field_rejected() {
        assert!(toml::from_str::<Preferences>(r#"bogus = 1"#).is_err());
    }

    // A single, self-contained fs test: `load`/`save` target one global
    // path, so the missing → save → load → corrupt sequence runs here to
    // avoid cross-test races on that file.
    #[test]
    fn save_load_and_corrupt_fallback() {
        // Save, read back — byte-for-byte the same values.
        let p = Preferences {
            last_page: Some("/tmp/some/page.md".to_string()),
            ..Preferences::default()
        };
        priv_save(&p).expect("save");
        let loaded = load();
        assert_eq!(loaded, p);

        // A corrupt file falls back to defaults (never blocks launch).
        std::fs::write(path(), "definitely not [toml]").expect("write corrupt");
        let d = load();
        assert_eq!(d, Preferences::default());

        // Clean up so we don't leave a stray file for other runs.
        let _ = std::fs::remove_file(path());
    }

    /// `save` is already pub; alias to keep the test local-reading.
    fn priv_save(prefs: &Preferences) -> Result<(), std::io::Error> {
        save(prefs)
    }
}
