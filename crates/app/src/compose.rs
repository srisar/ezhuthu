//! What the popup is composing: romanized words, the user's candidate picks,
//! and the Tamil phrase they produce. Pure logic, no Win32.
//!
//! The input line is split on spaces. Each word's transliterated part is its
//! last run of ASCII letters (as in the official engine); anything around it
//! (digits, punctuation) is kept verbatim. The last word is the one being
//! typed: its candidates fill the list, and Space or a digit commits a pick.

use std::collections::HashMap;

/// A word split into `prefix` + `core` (ASCII letters) + `suffix`.
#[derive(Debug, PartialEq)]
pub struct Parts<'a> {
    pub prefix: &'a str,
    pub core: &'a str,
    pub suffix: &'a str,
}

pub fn split_word(word: &str) -> Parts<'_> {
    let bytes = word.as_bytes();
    let Some(end) = bytes.iter().rposition(|b| b.is_ascii_alphabetic()) else {
        return Parts {
            prefix: word,
            core: "",
            suffix: "",
        };
    };
    let start = bytes[..end]
        .iter()
        .rposition(|b| !b.is_ascii_alphabetic())
        .map_or(0, |i| i + 1);
    Parts {
        prefix: &word[..start],
        core: &word[start..=end],
        suffix: &word[end + 1..],
    }
}

/// Cache key for a word's core: the model lowercases its input anyway.
pub fn key(core: &str) -> String {
    core.to_ascii_lowercase()
}

#[derive(Debug, Default)]
pub struct Composer {
    text: String,
    /// Word index -> (romanized word at the time, chosen Tamil). A pick only
    /// applies while the word at that index is still the same text.
    picks: HashMap<usize, (String, String)>,
    selected: usize,
}

impl Composer {
    pub fn clear(&mut self) {
        *self = Self::default();
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    /// Updates the input line. Resets the selection when the current word changed.
    pub fn set_text(&mut self, text: &str) {
        if self.current_word() != last_word(text) {
            self.selected = 0;
        }
        self.text = text.to_string();
        let words: Vec<&str> = self.text.split(' ').collect();
        self.picks
            .retain(|&i, (roman, _)| words.get(i).is_some_and(|w| w == roman));
    }

    pub fn current_word(&self) -> &str {
        last_word(&self.text)
    }

    fn current_index(&self) -> usize {
        self.text.matches(' ').count()
    }

    /// Lowercased cores of every word that needs transliterating.
    pub fn keys(&self) -> Vec<String> {
        self.text
            .split(' ')
            .map(|w| split_word(w).core)
            .filter(|c| !c.is_empty())
            .map(key)
            .collect()
    }

    pub fn current_key(&self) -> Option<String> {
        let core = split_word(self.current_word()).core;
        (!core.is_empty()).then(|| key(core))
    }

    pub fn selected(&self) -> usize {
        self.selected
    }

    pub fn select(&mut self, index: usize) {
        self.selected = index;
    }

    /// Moves the selection by `delta`, wrapping within `count` candidates.
    pub fn move_selection(&mut self, delta: isize, count: usize) {
        if count > 0 {
            self.selected = (self.selected as isize + delta).rem_euclid(count as isize) as usize;
        }
    }

    /// Records the selected candidate as the pick for the current word.
    pub fn commit_current(&mut self, candidates: Option<&[String]>) {
        let Some(tamil) = candidates.and_then(|c| c.get(self.selected)) else {
            return;
        };
        let word = self.current_word().to_string();
        if !split_word(&word).core.is_empty() {
            self.picks
                .insert(self.current_index(), (word, tamil.clone()));
        }
    }

    /// The Tamil phrase. `lookup(key)` gives a word's candidates (best
    /// first) if known; unknown words show as typed until results arrive.
    pub fn render<'a>(&self, lookup: impl Fn(&str) -> Option<&'a [String]>) -> String {
        let last = self.current_index();
        let mut out = String::new();
        for (i, word) in self.text.split(' ').enumerate() {
            if i > 0 {
                out.push(' ');
            }
            let parts = split_word(word);
            if parts.core.is_empty() {
                out.push_str(word);
                continue;
            }
            let rank = if i == last { self.selected } else { 0 };
            let tamil = match self.picks.get(&i) {
                Some((roman, tamil)) if roman == word => Some(tamil.as_str()),
                _ => lookup(&key(parts.core))
                    .and_then(|c| c.get(rank).or_else(|| c.first()))
                    .map(String::as_str),
            };
            out.push_str(parts.prefix);
            out.push_str(tamil.unwrap_or(parts.core));
            out.push_str(parts.suffix);
        }
        out
    }
}

fn last_word(text: &str) -> &str {
    text.rsplit(' ').next().unwrap_or("")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cands() -> HashMap<String, Vec<String>> {
        [
            ("amma", vec!["அம்மா", "ஆம்மா"]),
            ("kadal", vec!["கடல்", "கடால்"]),
            ("vanakkam", vec!["வணக்கம்", "வனக்கம்"]),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v.into_iter().map(String::from).collect()))
        .collect()
    }

    fn render(c: &Composer, map: &HashMap<String, Vec<String>>) -> String {
        c.render(|k| map.get(k).map(Vec::as_slice))
    }

    #[test]
    fn splits_words_like_the_official_engine() {
        assert_eq!(
            split_word("amma,"),
            Parts {
                prefix: "",
                core: "amma",
                suffix: ","
            }
        );
        assert_eq!(
            split_word("(kadal)"),
            Parts {
                prefix: "(",
                core: "kadal",
                suffix: ")"
            }
        );
        assert_eq!(
            split_word("123"),
            Parts {
                prefix: "123",
                core: "",
                suffix: ""
            }
        );
        // only the last letter run is transliterated
        assert_eq!(
            split_word("ab1cd"),
            Parts {
                prefix: "ab1",
                core: "cd",
                suffix: ""
            }
        );
    }

    #[test]
    fn renders_top1_and_keeps_punctuation() {
        let map = cands();
        let mut c = Composer::default();
        c.set_text("Amma, kadal! 42");
        assert_eq!(render(&c, &map), "அம்மா, கடல்! 42");
        assert_eq!(c.keys(), ["amma", "kadal"]);
    }

    #[test]
    fn unknown_words_show_as_typed() {
        let mut c = Composer::default();
        c.set_text("amma xyz");
        assert_eq!(render(&c, &cands()), "அம்மா xyz");
    }

    #[test]
    fn selection_applies_to_current_word_and_survives_commit() {
        let map = cands();
        let mut c = Composer::default();
        c.set_text("amma");
        c.move_selection(1, 2);
        assert_eq!(render(&c, &map), "ஆம்மா");
        c.commit_current(map.get("amma").map(Vec::as_slice));
        c.set_text("amma ");
        assert_eq!(c.selected(), 0, "new word starts at the top candidate");
        c.set_text("amma kadal");
        assert_eq!(render(&c, &map), "ஆம்மா கடல்");
    }

    #[test]
    fn editing_a_word_drops_its_pick() {
        let map = cands();
        let mut c = Composer::default();
        c.set_text("amma");
        c.select(1);
        c.commit_current(map.get("amma").map(Vec::as_slice));
        c.set_text("amm");
        c.set_text("amma");
        assert_eq!(render(&c, &map), "அம்மா");
    }

    #[test]
    fn selection_wraps() {
        let mut c = Composer::default();
        c.set_text("amma");
        c.move_selection(-1, 5);
        assert_eq!(c.selected(), 4);
        c.move_selection(1, 5);
        assert_eq!(c.selected(), 0);
    }
}
