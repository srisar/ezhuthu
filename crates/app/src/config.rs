//! `%APPDATA%\ezhuthu\config.toml`. Written with commented defaults on first run.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::Deserialize;

const DEFAULT_TOML: &str = r#"# ezhuthu settings. Use the tray menu's "Reload settings" after editing
# (changes to the model settings need a restart).

# Global hotkey that opens the popup: Ctrl / Alt / Shift / Win + a key.
hotkey = "Ctrl+Alt+T"

# How text reaches the target app:
#   "unicode"   types the characters directly (default)
#   "clipboard" pastes with Ctrl+V, for apps that mishandle typed Unicode.
#               Only the clipboard's text is restored afterwards.
insert_mode = "unicode"

# Candidates per word (beam size). Higher is slower.
beam = 5

# Folder with the model files. Default: "assets" next to ezhuthu.exe.
# assets_dir = 'C:\path\to\assets'
"#;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum InsertMode {
    #[default]
    Unicode,
    Clipboard,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub hotkey: String,
    pub insert_mode: InsertMode,
    pub beam: usize,
    pub assets_dir: Option<PathBuf>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            hotkey: "Ctrl+Alt+T".into(),
            insert_mode: InsertMode::Unicode,
            beam: ezhuthu_xlit::DEFAULT_BEAM,
            assets_dir: None,
        }
    }
}

impl Config {
    pub fn path() -> Result<PathBuf> {
        let appdata = std::env::var_os("APPDATA").context("APPDATA is not set")?;
        Ok(PathBuf::from(appdata).join("ezhuthu").join("config.toml"))
    }

    /// Loads the config, creating the file with defaults if it doesn't exist.
    pub fn load_or_create(path: &Path) -> Result<Self> {
        if !path.exists() {
            if let Some(dir) = path.parent() {
                std::fs::create_dir_all(dir)
                    .with_context(|| format!("creating {}", dir.display()))?;
            }
            std::fs::write(path, DEFAULT_TOML)
                .with_context(|| format!("writing {}", path.display()))?;
        }
        let text =
            std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        Self::parse(&text).with_context(|| format!("in {}", path.display()))
    }

    pub fn parse(text: &str) -> Result<Self> {
        let config: Config = toml::from_str(text)?;
        anyhow::ensure!((1..=16).contains(&config.beam), "beam must be 1..=16");
        Ok(config)
    }

    /// Model folder: the configured one, else `assets` next to the exe, else
    /// the workspace `assets/` (when running from a cargo build).
    pub fn assets_dir(&self) -> PathBuf {
        if let Some(dir) = &self.assets_dir {
            return dir.clone();
        }
        let beside_exe = std::env::current_exe()
            .ok()
            .and_then(|exe| exe.parent().map(|d| d.join("assets")));
        match beside_exe {
            Some(dir) if dir.join("vocab_src.json").exists() => dir,
            _ => Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_file_parses_to_defaults() {
        assert_eq!(Config::parse(DEFAULT_TOML).unwrap(), Config::default());
    }

    #[test]
    fn partial_file_keeps_other_defaults() {
        let c = Config::parse("insert_mode = \"clipboard\"").unwrap();
        assert_eq!(c.insert_mode, InsertMode::Clipboard);
        assert_eq!(c.hotkey, "Ctrl+Alt+T");
    }

    #[test]
    fn rejects_typos_and_bad_values() {
        assert!(Config::parse("hotkye = \"Ctrl+T\"").is_err());
        assert!(Config::parse("insert_mode = \"paste\"").is_err());
        assert!(Config::parse("beam = 0").is_err());
    }
}
