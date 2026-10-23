//! "Convert selection": turns romanized text into Tamil in one go. Pure
//! text logic; app.rs copies the selection, gets the words transliterated
//! by the engine and pastes the result.
//!
//! Every run of ASCII letters becomes its top Tamil word; everything else
//! (spaces, line breaks, digits, punctuation, text already in Tamil) is kept.

use std::collections::HashSet;

/// Longest selection converted, in characters.
pub const MAX_CHARS: usize = 20_000;
/// Most different words converted at once (each takes ~40 ms to transliterate).
pub const MAX_WORDS: usize = 300;

/// The runs of ASCII letters in `text`, as (byte range, lowercase key).
fn runs(text: &str) -> Vec<(std::ops::Range<usize>, String)> {
    let mut out = Vec::new();
    let mut start = None;
    for (i, c) in text
        .char_indices()
        .chain(std::iter::once((text.len(), ' ')))
    {
        match (c.is_ascii_alphabetic(), start) {
            (true, None) => start = Some(i),
            (false, Some(s)) => {
                out.push((s..i, text[s..i].to_ascii_lowercase()));
                start = None;
            }
            _ => {}
        }
    }
    out
}

/// The distinct words (lowercase keys) to transliterate, in order of appearance.
pub fn keys(text: &str) -> Vec<String> {
    let mut seen = HashSet::new();
    runs(text)
        .into_iter()
        .map(|(_, key)| key)
        .filter(|key| seen.insert(key.clone()))
        .collect()
}

/// `text` with each run of letters replaced by `tamil(key)`; a run whose
/// lookup gives nothing stays as typed.
pub fn convert(text: &str, tamil: impl Fn(&str) -> Option<String>) -> String {
    let mut out = String::with_capacity(text.len() * 3);
    let mut last = 0;
    for (range, key) in runs(text) {
        out.push_str(&text[last..range.start]);
        match tamil(&key) {
            Some(word) if !word.is_empty() => out.push_str(&word),
            _ => out.push_str(&text[range.clone()]),
        }
        last = range.end;
    }
    out.push_str(&text[last..]);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lookup(key: &str) -> Option<String> {
        match key {
            "vanakkam" => Some("வணக்கம்".into()),
            "amma" => Some("அம்மா".into()),
            "nanri" => Some("நன்றி".into()),
            _ => None,
        }
    }

    #[test]
    fn converts_each_word_and_keeps_the_rest() {
        assert_eq!(
            convert("Vanakkam amma, 2 times!\r\nnanri.", lookup),
            "வணக்கம் அம்மா, 2 times!\r\nநன்றி."
        );
    }

    #[test]
    fn leaves_tamil_and_unknown_words_alone() {
        assert_eq!(convert("அம்மா xyz amma", lookup), "அம்மா xyz அம்மா");
        assert_eq!(convert("", lookup), "");
        assert_eq!(convert("123 ...", lookup), "123 ...");
    }

    #[test]
    fn letters_next_to_digits_are_separate_words() {
        assert_eq!(convert("amma2nanri", lookup), "அம்மா2நன்றி");
    }

    #[test]
    fn keys_are_distinct_lowercase_in_order() {
        assert_eq!(
            keys("Amma vanakkam amma AMMA, nanri"),
            ["amma", "vanakkam", "nanri"]
        );
        assert!(keys("123 அம்மா").is_empty());
    }
}
