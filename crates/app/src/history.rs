//! The words the user has inserted, for predicting a word from its first
//! letters: `%APPDATA%\ezhuthu\history.tsv`, one `key<TAB>word<TAB>count
//! <TAB>last used` line per word (key = the romanized letters typed for it,
//! lowercase; last used = Unix seconds). Only Tamil words typed in Tamil
//! mode are kept (never in password fields: nothing is composed there).
//!
//! Written via a temporary file so it is never half-written; the least
//! used words make room when it is full.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};

use crate::learned;

/// Words kept at most; the least used make room for new ones.
const MAX_WORDS: usize = 20_000;

#[derive(Debug, Clone, PartialEq)]
struct Use {
    count: u32,
    last: u64,
}

#[derive(Debug, Default)]
pub struct History {
    words: HashMap<(String, String), Use>,
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

impl History {
    pub fn path() -> Result<PathBuf> {
        let appdata = std::env::var_os("APPDATA").context("APPDATA is not set")?;
        Ok(PathBuf::from(appdata).join("ezhuthu").join("history.tsv"))
    }

    /// Reads the file; a missing file is an empty history, and lines that
    /// don't parse are skipped.
    pub fn load(path: &Path) -> Result<Self> {
        let text = match std::fs::read_to_string(path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
        };
        Ok(Self::parse(&text))
    }

    fn parse(text: &str) -> Self {
        let mut words = HashMap::new();
        for line in text.lines().take(MAX_WORDS) {
            let mut fields = line.split('\t');
            let (Some(key), Some(word), Some(count), Some(last)) =
                (fields.next(), fields.next(), fields.next(), fields.next())
            else {
                continue;
            };
            let (Ok(count), Ok(last)) = (count.parse(), last.parse()) else {
                continue;
            };
            if learned::valid(key, word) {
                words.insert((key.to_string(), word.to_string()), Use { count, last });
            }
        }
        Self { words }
    }

    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.words.len()
    }

    /// Records that `word` was inserted for the letters `key`.
    pub fn record(&mut self, key: &str, word: &str) {
        self.record_at(key, word, now());
    }

    fn record_at(&mut self, key: &str, word: &str, at: u64) {
        if !learned::valid(key, word) {
            return;
        }
        let entry = (key.to_string(), word.to_string());
        if !self.words.contains_key(&entry) && self.words.len() >= MAX_WORDS {
            // Make room: forget the least used (then the oldest) word.
            if let Some(worst) = self
                .words
                .iter()
                .min_by_key(|(_, u)| (u.count, u.last))
                .map(|(k, _)| k.clone())
            {
                self.words.remove(&worst);
            }
        }
        let used = self
            .words
            .entry(entry)
            .or_insert(Use { count: 0, last: at });
        used.count = used.count.saturating_add(1);
        used.last = at;
    }

    /// Words once inserted for letters starting with `typed`, best first
    /// (most used, then most recent), leaving out `skip`; up to `limit`.
    pub fn predict(&self, typed: &str, skip: &[String], limit: usize) -> Vec<String> {
        let mut found: HashMap<&str, &Use> = HashMap::new();
        for ((key, word), used) in &self.words {
            if key.starts_with(typed) && !skip.contains(word) {
                let best = found.entry(word).or_insert(used);
                if (used.count, used.last) > (best.count, best.last) {
                    *best = used;
                }
            }
        }
        let mut ranked: Vec<(&str, &Use)> = found.into_iter().collect();
        ranked.sort_by_key(|(_, u)| std::cmp::Reverse((u.count, u.last)));
        ranked
            .into_iter()
            .take(limit)
            .map(|(w, _)| w.to_string())
            .collect()
    }

    pub fn clear(&mut self) {
        self.words.clear();
    }

    fn to_tsv(&self) -> String {
        let mut lines: Vec<String> = self
            .words
            .iter()
            .map(|((k, w), u)| format!("{k}\t{w}\t{}\t{}\n", u.count, u.last))
            .collect();
        lines.sort();
        lines.concat()
    }

    /// Writes the file, or deletes it when the history is empty.
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn predicts_words_typed_before_most_used_first() {
        let mut h = History::default();
        h.record_at("natchathiram", "நட்சத்திரம்", 10);
        h.record_at("natpu", "நட்பு", 20);
        h.record_at("natpu", "நட்பு", 30);
        h.record_at("amma", "அம்மா", 40);
        assert_eq!(h.predict("nat", &[], 3), ["நட்பு", "நட்சத்திரம்"]);
        assert_eq!(h.predict("natc", &[], 3), ["நட்சத்திரம்"]);
        // the exact letters count too, and words already shown are skipped
        assert_eq!(h.predict("amma", &[], 3), ["அம்மா"]);
        assert!(h.predict("amm", &["அம்மா".to_string()], 3).is_empty());
    }

    #[test]
    fn keeps_only_tamil_words_for_plain_keys() {
        let mut h = History::default();
        h.record("hello", "hello");
        h.record("Amma", "அம்மா");
        assert_eq!(h.len(), 0);
    }

    #[test]
    fn makes_room_by_forgetting_the_least_used() {
        let mut h = History::default();
        for i in 0..MAX_WORDS {
            let used = Use {
                count: if i == 7 { 1 } else { 2 },
                last: i as u64,
            };
            h.words.insert((format!("k{i}"), format!("அ{i}")), used);
        }
        h.record_at("kadal", "கடல்", 99);
        assert_eq!(h.len(), MAX_WORDS);
        assert!(!h.words.contains_key(&("k7".to_string(), "அ7".to_string())));
        assert_eq!(h.predict("kad", &[], 1), ["கடல்"]);
    }

    #[test]
    fn file_round_trip_skips_bad_lines() {
        let mut h = History::default();
        h.record_at("kadal", "கடல்", 5);
        h.record_at("kadal", "கடல்", 6);
        let text = format!("{}bad line\nx\tகடல்\tnotanumber\t1\n", h.to_tsv());
        let back = History::parse(&text);
        assert_eq!(back.words, h.words);

        let dir = std::env::temp_dir().join(format!("ezhuthu-history-{}", std::process::id()));
        let path = dir.join("history.tsv");
        h.save(&path).unwrap();
        assert_eq!(History::load(&path).unwrap().words, h.words);
        History::default().save(&path).unwrap();
        assert!(!path.exists(), "an empty history deletes the file");
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
