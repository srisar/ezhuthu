use std::collections::HashMap;
use std::path::Path;

use anyhow::{Context, Result};
use serde::Deserialize;

/// A fairseq dictionary as dumped by `tools/export/export_onnx.py`.
#[derive(Debug, Deserialize)]
pub struct Vocab {
    pub symbols: Vec<String>,
    pub bos: u32,
    pub pad: u32,
    pub eos: u32,
    pub unk: u32,
    pub lang_tokens: HashMap<String, u32>,
    #[serde(skip)]
    index: HashMap<String, u32>,
}

impl Vocab {
    pub fn load(path: &Path) -> Result<Self> {
        let text =
            std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        let mut vocab: Vocab =
            serde_json::from_str(&text).with_context(|| format!("parsing {}", path.display()))?;
        vocab.index = vocab
            .symbols
            .iter()
            .enumerate()
            .map(|(i, s)| (s.clone(), i as u32))
            .collect();
        Ok(vocab)
    }

    pub fn len(&self) -> usize {
        self.symbols.len()
    }

    pub fn is_empty(&self) -> bool {
        self.symbols.is_empty()
    }

    pub fn id(&self, symbol: &str) -> u32 {
        self.index.get(symbol).copied().unwrap_or(self.unk)
    }

    pub fn lang_token(&self, lang: &str) -> Option<u32> {
        self.lang_tokens.get(&format!("__{lang}__")).copied()
    }

    /// Joins generated characters, skipping specials (eos ends every hypothesis).
    pub fn decode(&self, tokens: &[u32]) -> String {
        tokens
            .iter()
            .filter(|&&t| t != self.eos && t != self.bos && t != self.pad)
            .map(|&t| self.symbols[t as usize].as_str())
            .collect()
    }
}
