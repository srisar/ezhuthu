//! `ta_word_freq.bin` reader (format: `tools/export/word_freq.py`).
//!
//! The file stays in memory as one buffer; lookups binary-search the sorted
//! offset table, so there is no per-word allocation or hash map.

use std::cmp::Ordering;
use std::path::Path;

use anyhow::{Context, Result, ensure};

use crate::rescore::WordProb;

const MAGIC: &[u8; 4] = b"EZWF";
const HEADER: usize = 12;

pub struct WordFreq {
    data: Vec<u8>,
    count: usize,
}

impl WordFreq {
    pub fn load(path: &Path) -> Result<Self> {
        let data = std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
        Self::from_bytes(data).with_context(|| format!("parsing {}", path.display()))
    }

    pub fn from_bytes(data: Vec<u8>) -> Result<Self> {
        ensure!(
            data.len() >= HEADER && &data[..4] == MAGIC,
            "not an ezhuthu word-freq file"
        );
        let version = u32_at(&data, 4);
        ensure!(version == 1, "unsupported word-freq version {version}");
        let count = u32_at(&data, 8) as usize;
        let wf = Self { data, count };
        ensure!(
            wf.records_start() <= wf.data.len(),
            "truncated offset table"
        );
        // Every record must fit; checked once so lookups can index freely.
        for i in 0..count {
            let off = wf.records_start() + u32_at(&wf.data, HEADER + 4 * i) as usize;
            ensure!(off < wf.data.len(), "record {i} out of bounds");
            let len = wf.data[off] as usize;
            ensure!(off + 1 + len + 4 <= wf.data.len(), "record {i} truncated");
        }
        Ok(wf)
    }

    pub fn len(&self) -> usize {
        self.count
    }

    pub fn is_empty(&self) -> bool {
        self.count == 0
    }

    fn records_start(&self) -> usize {
        HEADER + 4 * self.count
    }

    /// (encoded word, probability) of record `i`.
    fn record(&self, i: usize) -> (&[u8], f32) {
        let off = self.records_start() + u32_at(&self.data, HEADER + 4 * i) as usize;
        let len = self.data[off] as usize;
        let word = &self.data[off + 1..off + 1 + len];
        let p = &self.data[off + 1 + len..off + 5 + len];
        (word, f32::from_le_bytes([p[0], p[1], p[2], p[3]]))
    }
}

impl WordProb for WordFreq {
    fn prob(&self, word: &str) -> Option<f64> {
        let key = encode_word(word)?;
        let (mut lo, mut hi) = (0, self.count);
        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            let (w, p) = self.record(mid);
            match w.cmp(&key) {
                Ordering::Less => lo = mid + 1,
                Ordering::Greater => hi = mid,
                Ordering::Equal => return Some(p as f64),
            }
        }
        None
    }
}

impl WordFreq {
    /// The most frequent words that start with `prefix` and are longer than
    /// it, most frequent first: up to `limit` of them, from at most
    /// `MAX_SCAN` words starting with the prefix (in dictionary order).
    pub fn completions(&self, prefix: &str, limit: usize) -> Vec<(String, f32)> {
        const MAX_SCAN: usize = 200_000;
        let Some(key) = encode_word(prefix) else {
            return Vec::new();
        };
        if key.is_empty() || limit == 0 {
            return Vec::new();
        }
        // First record not less than the prefix.
        let (mut lo, mut hi) = (0, self.count);
        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            if self.record(mid).0 < key.as_slice() {
                lo = mid + 1;
            } else {
                hi = mid;
            }
        }
        let mut best: Vec<(f32, usize)> = Vec::with_capacity(limit + 1);
        for i in (lo..self.count).take(MAX_SCAN) {
            let (word, p) = self.record(i);
            if !word.starts_with(&key) {
                break;
            }
            if word.len() == key.len() {
                continue;
            }
            if best.len() < limit || p > best[best.len() - 1].0 {
                let at = best.partition_point(|&(q, _)| q >= p);
                best.insert(at, (p, i));
                best.truncate(limit);
            }
        }
        best.into_iter()
            .map(|(p, i)| (decode_word(self.record(i).0), p))
            .collect()
    }
}

/// The inverse of `encode_word`.
fn decode_word(bytes: &[u8]) -> String {
    bytes
        .iter()
        .map(|&b| {
            if b < 0x80 {
                b as char
            } else {
                char::from_u32(0x0B80 + (b as u32 - 0x80)).unwrap_or(char::REPLACEMENT_CHARACTER)
            }
        })
        .collect()
}

/// One byte per character: the Tamil block U+0B80..U+0BFF maps to
/// 0x80 + offset, ASCII stays; anything else can't be in the file.
fn encode_word(word: &str) -> Option<Vec<u8>> {
    word.chars()
        .map(|c| match c as u32 {
            cp @ 0..0x80 => Some(cp as u8),
            cp @ 0x0B80..=0x0BFF => Some((0x80 + cp - 0x0B80) as u8),
            _ => None,
        })
        .collect()
}

fn u32_at(data: &[u8], off: usize) -> u32 {
    u32::from_le_bytes([data[off], data[off + 1], data[off + 2], data[off + 3]])
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Builds a file the way word_freq.py does.
    fn build(words: &[(&str, f32)]) -> Vec<u8> {
        let mut recs: Vec<(Vec<u8>, f32)> = words
            .iter()
            .map(|(w, p)| (encode_word(w).unwrap(), *p))
            .collect();
        recs.sort_by(|a, b| a.0.cmp(&b.0));
        let mut offsets = Vec::new();
        let mut body = Vec::new();
        for (w, p) in &recs {
            offsets.extend_from_slice(&(body.len() as u32).to_le_bytes());
            body.push(w.len() as u8);
            body.extend_from_slice(w);
            body.extend_from_slice(&p.to_le_bytes());
        }
        let mut data = MAGIC.to_vec();
        data.extend_from_slice(&1u32.to_le_bytes());
        data.extend_from_slice(&(recs.len() as u32).to_le_bytes());
        data.extend(offsets);
        data.extend(body);
        data
    }

    #[test]
    fn finds_words_and_misses_others() {
        let wf =
            WordFreq::from_bytes(build(&[("தமிழ்", 0.5), ("அம்மா", 0.25), ("கடல்", 0.125)])).unwrap();
        assert_eq!(wf.prob("அம்மா"), Some(0.25));
        assert_eq!(wf.prob("தமிழ்"), Some(0.5));
        assert_eq!(wf.prob("கடல்"), Some(0.125));
        assert_eq!(wf.prob("கடால்"), None);
        assert_eq!(wf.prob("नमस्ते"), None); // outside the Tamil block
    }

    #[test]
    fn completes_prefixes_by_frequency() {
        let wf = WordFreq::from_bytes(build(&[
            ("நட", 0.9),
            ("நடனம்", 0.2),
            ("நடந்தது", 0.4),
            ("நடை", 0.3),
            ("நம்", 0.8),
        ]))
        .unwrap();
        let words: Vec<String> = wf
            .completions("நட", 2)
            .into_iter()
            .map(|(w, _)| w)
            .collect();
        assert_eq!(
            words,
            ["நடந்தது", "நடை"],
            "longer words only, most frequent first"
        );
        assert!(wf.completions("கட", 3).is_empty());
        assert!(
            wf.completions("नम", 3).is_empty(),
            "outside the Tamil block"
        );
    }

    #[test]
    fn rejects_bad_files() {
        assert!(WordFreq::from_bytes(b"nope".to_vec()).is_err());
        let mut data = build(&[("அம்மா", 0.25)]);
        data.truncate(data.len() - 2);
        assert!(WordFreq::from_bytes(data).is_err());
    }
}
