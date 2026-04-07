use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

const DEFAULT_WIDTH: i32 = 900;
const DEFAULT_HEIGHT: i32 = 700;
const KEYRING_SERVICE: &str = "gemini-lite";
const KEYRING_USER: &str = "api-key";

// ── Window state persistence ────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WindowState {
    pub width: i32,
    pub height: i32,
    pub pos_x: Option<i32>,
    pub pos_y: Option<i32>,
}

impl Default for WindowState {
    fn default() -> Self {
        Self {
            width: DEFAULT_WIDTH,
            height: DEFAULT_HEIGHT,
            pos_x: None,
            pos_y: None,
        }
    }
}

pub fn config_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("XDG_CONFIG_HOME") {
        return Path::new(&dir).join("gemini-lite");
    }
    std::env::var("HOME")
        .map(|h| Path::new(&h).join(".config").join("gemini-lite"))
        .unwrap_or_else(|_| PathBuf::from("."))
}

fn state_path() -> PathBuf {
    config_dir().join("window-state.json")
}

pub fn load_window_state() -> WindowState {
    match fs::read_to_string(state_path()).and_then(|s| Ok(serde_json::from_str(&s)?)) {
        Ok(state) => state,
        Err(e) => {
            log::debug!("no persisted window state ({e}), using defaults");
            WindowState::default()
        }
    }
}

pub fn save_window_state(state: &WindowState) -> Result<()> {
    let dir = config_dir();
    fs::create_dir_all(&dir)
        .with_context(|| format!("cannot create config dir {}", dir.display()))?;

    let json = serde_json::to_string_pretty(state).context("serialization failed")?;
    fs::write(state_path(), json).context("cannot write window-state.json")?;
    log::debug!(
        "window state saved: {}x{} @ ({:?},{:?})",
        state.width,
        state.height,
        state.pos_x,
        state.pos_y
    );
    Ok(())
}

// ── API key — Secret Service primary, file fallback ─────────────────────────

fn key_file_path() -> PathBuf {
    config_dir().join("api-key")
}

/// Load order: env var -> GNOME Keyring -> plain file in XDG_CONFIG_HOME.
pub fn load_api_key() -> Option<String> {
    if let Ok(k) = std::env::var("GEMINI_API_KEY") {
        let k = k.trim().to_string();
        if !k.is_empty() {
            log::debug!("API key loaded from environment");
            return Some(k);
        }
    }

    if let Some(k) = keyring::Entry::new(KEYRING_SERVICE, KEYRING_USER)
        .ok()
        .and_then(|e| e.get_password().ok())
        .filter(|k| !k.trim().is_empty())
    {
        log::debug!("API key loaded from system keyring");
        return Some(k.trim().to_string());
    }

    if let Some(k) = fs::read_to_string(key_file_path())
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|k| !k.is_empty())
    {
        log::debug!("API key loaded from config file (fallback)");
        return Some(k);
    }

    None
}

/// Attempt Secret Service first; if the daemon is absent, fall back to a
/// mode-0600 plain-text file under XDG_CONFIG_HOME.
pub fn save_api_key(key: &str) -> Result<()> {
    match keyring::Entry::new(KEYRING_SERVICE, KEYRING_USER)
        .context("cannot construct keyring entry")?
        .set_password(key)
    {
        Ok(()) => {
            log::info!("API key saved to system keyring");
            Ok(())
        }
        Err(e) => {
            log::warn!("keyring unavailable ({e:#}), falling back to config file");
            persist_key_to_file(key)
        }
    }
}

fn persist_key_to_file(key: &str) -> Result<()> {
    let dir = config_dir();
    fs::create_dir_all(&dir)
        .with_context(|| format!("cannot create config dir {}", dir.display()))?;

    let path = key_file_path();
    fs::write(&path, key).with_context(|| format!("cannot write {}", path.display()))?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600))
            .context("cannot chmod api-key file")?;
    }

    log::info!("API key saved to {}", path.display());
    Ok(())
}
