//! IndicXlit Roman→Tamil transliteration on tract (pure Rust ONNX).
//!
//! Assets come from `tools/export/export_onnx.py`: `xlit_encoder.onnx`,
//! `xlit_decoder.onnx`, `vocab_src.json`, `vocab_tgt.json`.

mod beam;
mod rescore;
mod vocab;

use std::path::Path;
use std::sync::Arc;

use anyhow::{Context, Result, bail};
use tract_onnx::prelude::*;

pub use beam::{Hypo, MAX_LEN};
pub use rescore::{ALPHA as RESCORE_ALPHA, WordProb, rescore};
pub use vocab::Vocab;

pub const DEFAULT_BEAM: usize = 5;
const TARGET_LANG: &str = "ta";

#[derive(Debug, Clone, PartialEq)]
pub struct Candidate {
    pub text: String,
    /// fairseq score: mean natural-log probability per generated token.
    pub score: f32,
}

type Plan = Arc<TypedRunnableModel>;

pub struct Transliterator {
    encoder: Plan,
    decoder: Plan,
    src: Vocab,
    tgt: Vocab,
    lang_tok: u32,
    beam: usize,
}

impl Transliterator {
    pub fn load(assets: &Path) -> Result<Self> {
        let src = Vocab::load(&assets.join("vocab_src.json"))?;
        let tgt = Vocab::load(&assets.join("vocab_tgt.json"))?;
        let lang_tok = src
            .lang_token(TARGET_LANG)
            .context("vocab_src.json has no __ta__ token")?;
        Ok(Self {
            encoder: load_plan(&assets.join("xlit_encoder.onnx"))?,
            decoder: load_plan(&assets.join("xlit_decoder.onnx"))?,
            src,
            tgt,
            lang_tok,
            beam: DEFAULT_BEAM,
        })
    }

    pub fn with_beam(mut self, beam: usize) -> Self {
        self.beam = beam.max(1);
        self
    }

    /// Source ids as the official engine builds them: `[__ta__, c1..cn, </s>]`
    /// from the last run of ASCII letters, lowercased.
    pub fn encode_source(&self, word: &str) -> Option<Vec<u32>> {
        let lower = word.trim().to_ascii_lowercase();
        let run = lower
            .split(|c: char| !c.is_ascii_lowercase())
            .rfind(|s| !s.is_empty())?;
        let mut ids = Vec::with_capacity(run.len() + 2);
        ids.push(self.lang_tok);
        let mut buf = [0u8; 4];
        ids.extend(run.chars().map(|c| self.src.id(c.encode_utf8(&mut buf))));
        ids.push(self.src.eos);
        Some(ids)
    }

    /// Top-`beam` Tamil candidates for one romanized word, best first.
    pub fn transliterate(&self, word: &str) -> Result<Vec<Candidate>> {
        let Some(src) = self.encode_source(word) else {
            bail!("no latin letters in {word:?}");
        };
        let src_tensor = tensor_i64(&[1, src.len()], src.iter().map(|&t| t as i64))?;
        let mut cross = self.encoder.run(tvec![src_tensor.into()])?;
        let (cross_k, cross_v) = (cross.remove(0), cross.remove(0));
        // cross_k: [L, 1, H, S, hd]; the self-attention cache shares L, H, hd.
        let (layers, heads, head_dim) = match cross_k.shape() {
            &[l, 1, h, _, d] => (l, h, d),
            other => bail!("unexpected cross_k shape {other:?}"),
        };

        // Self-attention cache [L, n, H, P, hd] from the previous step.
        let mut cache: Option<(TValue, TValue)> = None;
        let vocab = self.tgt.len();
        let sp = beam::Specials {
            eos: self.tgt.eos,
            pad: self.tgt.pad,
        };
        let hypos = beam::beam_search(
            |prefixes, parents| {
                let n = prefixes.len();
                let (past_k, past_v) = match &cache {
                    None => {
                        let empty = Tensor::zero::<f32>(&[layers, n, heads, 0, head_dim])?;
                        (empty.clone().into(), empty.into())
                    }
                    Some((k, v)) => (
                        gather_rows(k, parents)?.into(),
                        gather_rows(v, parents)?.into(),
                    ),
                };
                let tokens =
                    tensor_i64(&[n, 1], prefixes.iter().map(|p| *p.last().unwrap() as i64))?;
                let mut out = self.decoder.run(tvec![
                    tokens.into(),
                    past_k,
                    past_v,
                    cross_k.clone(),
                    cross_v.clone(),
                ])?;
                let lprobs = out[0].try_as_plain_ram()?.as_slice::<f32>()?.to_vec();
                cache = Some((out.remove(1), out.remove(1)));
                Ok(lprobs)
            },
            vocab,
            sp,
            self.beam,
        )?;
        Ok(hypos
            .into_iter()
            .map(|h| Candidate {
                text: self.tgt.decode(&h.tokens),
                score: h.score,
            })
            .collect())
    }
}

fn tensor_i64(shape: &[usize], data: impl Iterator<Item = i64>) -> Result<Tensor> {
    let data: Vec<i64> = data.collect();
    Tensor::from_shape(shape, &data)
}

/// Selects batch rows (axis 1) of a `[L, B, H, P, hd]` cache: row `r` of the
/// result is row `parents[r]` of `t`. This is the beam reorder.
fn gather_rows(t: &TValue, parents: &[usize]) -> Result<Tensor> {
    let shape = t.shape();
    let (layers, batch) = (shape[0], shape[1]);
    let block: usize = shape[2..].iter().product();
    let src = t.try_as_plain_ram()?;
    let src = src.as_slice::<f32>()?;
    let mut data = Vec::with_capacity(layers * parents.len() * block);
    for l in 0..layers {
        for &p in parents {
            let start = (l * batch + p) * block;
            data.extend_from_slice(&src[start..start + block]);
        }
    }
    let mut new_shape = shape.to_vec();
    new_shape[1] = parents.len();
    Tensor::from_shape(&new_shape, &data)
}

/// Both graphs declare their dynamic axes (S, B, P) as ONNX dim params,
/// which tract turns into symbols, so no input facts are needed.
fn load_plan(path: &Path) -> Result<Plan> {
    tract_onnx::onnx()
        .model_for_path(path)
        .with_context(|| format!("loading {}", path.display()))?
        .into_optimized()
        .with_context(|| format!("optimizing {}", path.display()))?
        .into_runnable()
        .with_context(|| format!("planning {}", path.display()))
}
