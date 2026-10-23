//! Suggestions the user picked over the model's first choice, remembered per
//! romanized word in `%APPDATA%\ezhuthu\learned.tsv` and shown first next time.
//!
//! The latest pick wins: picking the model's own first choice again forgets
//! the word. Only Tamil picks are kept; the file is one `key<TAB>word` line
//! per word, written via a temporary file so it is never half-written.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

/// Words remembered at most; picks for new words beyond this are not kept.
const MAX_WORDS: usize = 50_000;

#[derive(Debug, Default)]
pub struct Learned {
    words: HashMap<String, String>,
}

impl Learned {
    pub fn path() -> Result<PathBuf> {
        let appdata = std::env::var_os("APPDATA").context("APPDATA is not set")?;
        Ok(PathBuf::from(appdata).join("ezhuthu").join("learned.tsv"))
    }

    /// Reads the file; a missing file is an empty list, and lines that
    /// don't look like a key and a Tamil word are skipped.
    pub fn load(path: &Path) -> Result<Self> {
        let text = match std::fs::read_to_string(path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
        };
        Ok(Self::parse(&text))
    }

    fn parse(text: &str) -> Self {
        let words = text
            .lines()
            .filter_map(|line| line.split_once('\t'))
            .filter(|(key, word)| valid(key, word))
            .take(MAX_WORDS)
            .map(|(key, word)| (key.to_string(), word.to_string()))
            .collect();
        Self { words }
    }

    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.words.len()
    }

    /// Moves the remembered word for `key` to the front of `candidates`
    /// (adding it if the model no longer offers it).
    pub fn apply(&self, key: &str, candidates: &mut Vec<String>) {
        let Some(word) = self.words.get(key) else {
            return;
        };
        match candidates.iter().position(|c| c == word) {
            Some(0) => {}
            Some(i) => candidates[..=i].rotate_right(1),
            None => candidates.insert(0, word.clone()),
        }
    }

    /// Records that `chosen` was inserted for `key`, where the model's first
    /// choice was `model_top`. True if anything changed.
    pub fn record(&mut self, key: &str, chosen: &str, model_top: Option<&str>) -> bool {
        if model_top == Some(chosen) {
            return self.words.remove(key).is_some();
        }
        if !valid(key, chosen)
            || self.words.get(key).map(String::as_str) == Some(chosen)
            || (self.words.len() >= MAX_WORDS && !self.words.contains_key(key))
        {
            return false;
        }
        self.words.insert(key.to_string(), chosen.to_string());
        true
    }

    pub fn clear(&mut self) {
        self.words.clear();
    }

    fn to_tsv(&self) -> String {
        let mut lines: Vec<String> = self
            .words
            .iter()
            .map(|(k, w)| format!("{k}\t{w}\n"))
            .collect();
        lines.sort();
        lines.concat()
    }

    /// Writes the file, or deletes it when nothing is remembered.
    pub fn save(&self, path: &Path) -> Result<()> {
        if self.words.is_empty() {
            return match std::fs::remove_file(path) {
                Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
                    Err(e).with_context(|| format!("deleting {}", path.display()))
                }
                _ => Ok(()),
            };
        }
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
        }
        let tmp = path.with_extension("tsv.tmp");
        std::fs::write(&tmp, self.to_tsv())
            .with_context(|| format!("writing {}", tmp.display()))?;
        std::fs::rename(&tmp, path).with_context(|| format!("replacing {}", path.display()))
    }
}

/// A key is a lowercase romanized word; a word is Tamil text on one line.
fn valid(key: &str, word: &str) -> bool {
    !key.is_empty()
        && key.bytes().all(|b| b.is_ascii_lowercase())
        && word.chars().any(|c| ('\u{0B80}'..='\u{0BFF}').contains(&c))
        && !word.chars().any(char::is_control)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn list(words: &[&str]) -> Vec<String> {
        words.iter().map(|w| w.to_string()).collect()
    }

    #[test]
    fn a_pick_comes_first_next_time() {
        let mut l = Learned::default();
        assert!(l.record("thamil", "தமில்", Some("தமிழ்")));
        let mut c = list(&["தமிழ்", "தாமில்", "தமில்"]);
        l.apply("thamil", &mut c);
        assert_eq!(c, list(&["தமில்", "தமிழ்", "தாமில்"]));
        // other words are untouched
        let mut other = list(&["அம்மா", "ஆம்மா"]);
        l.apply("amma", &mut other);
        assert_eq!(other, list(&["அம்மா", "ஆம்மா"]));
    }

    #[test]
    fn a_pick_the_model_no_longer_offers_is_added() {
        let mut l = Learned::default();
        l.record("kadal", "கடல்", Some("காதல்"));
        let mut c = list(&["காதல்", "கதல்"]);
        l.apply("kadal", &mut c);
        assert_eq!(c, list(&["கடல்", "காதல்", "கதல்"]));
    }

    #[test]
    fn picking_the_models_choice_again_forgets() {
        let mut l = Learned::default();
        l.record("kadal", "கடல்", Some("காதல்"));
        assert!(
            !l.record("kadal", "கடல்", Some("காதல்")),
            "same pick: no change"
        );
        assert!(l.record("kadal", "காதல்", Some("காதல்")));
        assert_eq!(l.len(), 0);
        assert!(!l.record("kadal", "காதல்", Some("காதல்")));
    }

    #[test]
    fn keeps_only_tamil_picks_for_plain_keys() {
        let mut l = Learned::default();
        assert!(
            !l.record("hello", "hello", Some("ஹலோ")),
            "English isn't learned"
        );
        assert!(!l.record("Kadal", "கடல்", None), "keys are lowercase");
        assert!(!l.record("kadal", "கடல்\nx", None));
        assert_eq!(l.len(), 0);
    }

    #[test]
    fn file_round_trip_skips_bad_lines() {
        let l = Learned::parse("kadal\tகடல்\nbad line\nHELLO\tஹலோ\nthamil\tதமில்\n");
        assert_eq!(l.len(), 2);
        assert_eq!(Learned::parse(&l.to_tsv()).words, l.words);

        let dir = std::env::temp_dir().join(format!("ezhuthu-learned-{}", std::process::id()));
        let path = dir.join("learned.tsv");
        l.save(&path).unwrap();
        assert_eq!(Learned::load(&path).unwrap().words, l.words);
        Learned::default().save(&path).unwrap();
        assert!(!path.exists(), "empty list deletes the file");
        assert_eq!(Learned::load(&path).unwrap().len(), 0);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
