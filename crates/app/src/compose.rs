//! The word being typed in Tamil mode and the Tamil text it produces.
//! Pure logic, no Win32.
//!
//! A word's transliterated part is its last run of ASCII letters (as in the
//! official engine); anything around it (digits, punctuation) is kept
//! verbatim, so "amma," becomes "அம்மா,".

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

#[derive(Debug, Default)]
pub struct Composer {
    text: String,
    selected: usize,
}

impl Composer {
    pub fn clear(&mut self) {
        *self = Self::default();
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }

    pub fn push(&mut self, c: char) {
        self.edit(|t| t.push(c));
    }

    pub fn pop(&mut self) {
        self.edit(|t| {
            t.pop();
        });
    }

    /// Applies an edit; the selection resets when the transliterated part changes.
    fn edit(&mut self, f: impl FnOnce(&mut String)) {
        let before = self.key();
        f(&mut self.text);
        if self.key() != before {
            self.selected = 0;
        }
    }

    /// Cache key for the word's core: lowercased, as the model sees it.
    pub fn key(&self) -> Option<String> {
        let core = split_word(&self.text).core;
        (!core.is_empty()).then(|| core.to_ascii_lowercase())
    }

    /// The transliterated part as typed (case kept), offered as the English option.
    pub fn core(&self) -> &str {
        split_word(&self.text).core
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

    /// The word in Tamil with the selected candidate, or as typed while the
    /// candidates are unknown.
    pub fn render(&self, candidates: Option<&[String]>) -> String {
        let parts = split_word(&self.text);
        let tamil = candidates
            .and_then(|c| c.get(self.selected).or_else(|| c.first()))
            .map_or(parts.core, String::as_str);
        format!("{}{tamil}{}", parts.prefix, parts.suffix)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cands(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    fn typed(s: &str) -> Composer {
        let mut c = Composer::default();
        s.chars().for_each(|ch| c.push(ch));
        c
    }

    #[test]
    fn splits_words_like_the_official_engine() {
        let p = |prefix, core, suffix| Parts {
            prefix,
            core,
            suffix,
        };
        assert_eq!(split_word("amma,"), p("", "amma", ","));
        assert_eq!(split_word("(kadal)"), p("(", "kadal", ")"));
        assert_eq!(split_word("123"), p("123", "", ""));
        // only the last letter run is transliterated
        assert_eq!(split_word("ab1cd"), p("ab1", "cd", ""));
    }

    #[test]
    fn renders_selected_candidate_and_keeps_punctuation() {
        let c = typed("Amma,");
        assert_eq!(c.key().as_deref(), Some("amma"));
        let list = cands(&["அம்மா", "ஆம்மா"]);
        assert_eq!(c.render(Some(&list)), "அம்மா,");
        let mut c = c;
        c.move_selection(1, 2);
        assert_eq!(c.render(Some(&list)), "ஆம்மா,");
    }

    #[test]
    fn english_option_keeps_case_and_punctuation() {
        let mut c = typed("Hello,");
        assert_eq!(c.core(), "Hello");
        let list = vec!["ஹலோ".to_string(), c.core().to_string()];
        c.select(1);
        assert_eq!(c.render(Some(&list)), "Hello,");
    }

    #[test]
    fn shows_typed_text_until_candidates_arrive() {
        assert_eq!(typed("amma").render(None), "amma");
        assert_eq!(typed("amma").render(Some(&[])), "amma");
    }

    #[test]
    fn selection_resets_when_the_word_changes_but_not_for_punctuation() {
        let mut c = typed("amma");
        c.select(2);
        c.push(',');
        assert_eq!(c.selected(), 2, "punctuation keeps the pick");
        c.pop();
        c.pop();
        assert_eq!(c.selected(), 0, "editing letters resets it");
        assert_eq!(c.text(), "amm");
    }

    #[test]
    fn selection_wraps() {
        let mut c = typed("amma");
        c.move_selection(-1, 5);
        assert_eq!(c.selected(), 4);
        c.move_selection(1, 5);
        assert_eq!(c.selected(), 0);
    }

    #[test]
    fn pop_on_empty_is_harmless() {
        let mut c = Composer::default();
        c.pop();
        assert!(c.is_empty());
        assert_eq!(c.key(), None);
    }
}
