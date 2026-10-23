//! `%APPDATA%\ezhuthu\config.toml`. The Settings window edits it; it is
//! written with commented defaults on first run.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::Deserialize;

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
    /// Converts the selected text to Tamil (see `convert`); empty: off.
    pub convert_hotkey: String,
    pub insert_mode: InsertMode,
    pub beam: usize,
    /// Learn the suggestions the user picks (see `learned`).
    pub remember_picks: bool,
    pub assets_dir: Option<PathBuf>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            hotkey: "Ctrl+Alt+T".into(),
            convert_hotkey: "Ctrl+Alt+Shift+T".into(),
            insert_mode: InsertMode::Unicode,
            beam: ezhuthu_xlit::DEFAULT_BEAM,
            remember_picks: true,
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
            std::fs::write(path, Config::default().to_toml())
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

    /// The settings file's text, with comments explaining each setting.
    pub fn to_toml(&self) -> String {
        let quote = |s: &str| toml::Value::String(s.to_string()).to_string();
        let mode = match self.insert_mode {
            InsertMode::Unicode => "unicode",
            InsertMode::Clipboard => "clipboard",
        };
        let assets = match &self.assets_dir {
            Some(d) => format!("assets_dir = {}\n", quote(&d.display().to_string())),
            None => "# assets_dir = 'C:\\path\\to\\assets'\n".to_string(),
        };
        format!(
            "# ezhuthu settings. Change them from the tray menu: Settings...\n\
             # (this file is rewritten when you do).\n\
             \n\
             # Turns Tamil mode on and off: Ctrl / Alt / Shift / Win + a key.\n\
             hotkey = {}\n\
             \n\
             # Converts the selected text (English letters) to Tamil, in any app.\n\
             # Empty (\"\") turns it off.\n\
             convert_hotkey = {}\n\
             \n\
             # How text reaches the app: \"unicode\" types the characters (default);\n\
             # \"clipboard\" pastes with Ctrl+V, for apps that garble typed Tamil.\n\
             insert_mode = {}\n\
             \n\
             # Suggestions per word (beam size). Higher is slower.\n\
             beam = {}\n\
             \n\
             # Show the suggestion you picked for a word first next time\n\
             # (kept in learned.tsv next to this file).\n\
             remember_picks = {}\n\
             \n\
             # Folder with the model files. Default: \"assets\" next to ezhuthu.exe.\n\
             {assets}",
            quote(&self.hotkey),
            quote(&self.convert_hotkey),
            quote(mode),
            self.beam,
            self.remember_picks,
        )
    }

    /// Writes the settings file (via a temporary file, so it is never half-written).
    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
        }
        let tmp = path.with_extension("toml.tmp");
        std::fs::write(&tmp, self.to_toml())
            .with_context(|| format!("writing {}", tmp.display()))?;
        std::fs::rename(&tmp, path).with_context(|| format!("replacing {}", path.display()))
    }

    /// Model folder: the configured one, else `assets` next to the exe.
    ///
    /// Dev builds (debug assertions on: `dev` and `fast` profiles) also fall
    /// back to the workspace `assets/`. Release builds never look at that
    /// compile-time path, so nothing planted there on another machine loads.
    pub fn assets_dir(&self) -> PathBuf {
        if let Some(dir) = &self.assets_dir {
            return dir.clone();
        }
        let beside_exe = std::env::current_exe()
            .ok()
            .and_then(|exe| exe.parent().map(|d| d.join("assets")))
            .unwrap_or_else(|| PathBuf::from("assets"));
        if cfg!(debug_assertions) && !beside_exe.join("vocab_src.json").exists() {
            return Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets");
        }
        beside_exe
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_file_parses_to_defaults() {
        assert_eq!(
            Config::parse(&Config::default().to_toml()).unwrap(),
            Config::default()
        );
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

    #[test]
    fn saved_settings_read_back_identically() {
        let c = Config {
            hotkey: "Ctrl+Shift+F5".into(),
            convert_hotkey: String::new(),
            insert_mode: InsertMode::Clipboard,
            beam: 7,
            remember_picks: false,
            assets_dir: Some(PathBuf::from(r#"D:\it's "odd"\assets"#)),
        };
        assert_eq!(Config::parse(&c.to_toml()).unwrap(), c);
        let dir = std::env::temp_dir().join(format!("ezhuthu-cfg-{}", std::process::id()));
        let path = dir.join("config.toml");
        c.save(&path).unwrap();
        assert_eq!(Config::load_or_create(&path).unwrap(), c);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
