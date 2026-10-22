//! Port of fairseq's `SequenceGenerator._generate` + `BeamSearch.step` for a
//! single sentence, matching `tools/export/xlit_ref.py::beam_search`.
//!
//! fairseq defaults used by IndicXlit: lenpen 1, normalized scores,
//! unk penalty 0, min_len 1, max_len_b 200.

use anyhow::Result;

pub const MAX_LEN: usize = 200;

#[derive(Debug, Clone, Copy)]
pub struct Specials {
    pub eos: u32,
    pub pad: u32,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Hypo {
    /// Generated tokens, ending with eos.
    pub tokens: Vec<u32>,
    /// Sum of log-probs (natural log) divided by the number of tokens.
    pub score: f32,
}

/// `step(prefixes, parents)` gets `n` equal-length prefixes (each starting
/// with eos) and returns `n * vocab` next-token log-probs, row-major.
/// `parents[r]` is the row of the previous call that prefix `r` extends
/// (empty on the first call), so a KV cache can be reordered to match.
pub fn beam_search<F>(mut step: F, vocab: usize, sp: Specials, beam: usize) -> Result<Vec<Hypo>>
where
    F: FnMut(&[Vec<u32>], &[usize]) -> Result<Vec<f32>>,
{
    let eos = sp.eos as usize;
    let mut prefixes: Vec<Vec<u32>> = vec![vec![sp.eos]];
    let mut scores: Vec<f32> = vec![0.0];
    let mut parents: Vec<usize> = Vec::new();
    let mut finalized: Vec<Hypo> = Vec::with_capacity(beam);
    let cand_size = 2 * beam;

    for step_idx in 0..=MAX_LEN {
        // At step 0 every beam holds the same prefix, so one row is enough
        // (fairseq computes all rows but only reads the first).
        let mut lprobs = step(&prefixes, &parents)?;
        debug_assert_eq!(lprobs.len(), prefixes.len() * vocab);
        for row in lprobs.chunks_mut(vocab) {
            for v in row.iter_mut() {
                if v.is_nan() {
                    *v = f32::NEG_INFINITY;
                }
            }
            row[sp.pad as usize] = f32::NEG_INFINITY;
            if step_idx >= MAX_LEN {
                for (i, v) in row.iter_mut().enumerate() {
                    if i != eos {
                        *v = f32::NEG_INFINITY;
                    }
                }
            }
            if step_idx < 1 {
                row[eos] = f32::NEG_INFINITY;
            }
            // unk penalty is 0: nothing to subtract
        }

        // BeamSearch.step: cumulative scores, then top 2*beam over beam x vocab.
        let mut flat: Vec<(f32, usize)> = lprobs
            .iter()
            .enumerate()
            .map(|(i, &lp)| (lp + scores[i / vocab], i))
            .collect();
        let k = cand_size.min(flat.len() - 1);
        // torch.topk order: descending score; break exact ties by index.
        flat.select_nth_unstable_by(k - 1, cmp_desc);
        flat.truncate(k);
        flat.sort_unstable_by(cmp_desc);

        let is_eos = |&(s, i): &(f32, usize)| i % vocab == eos && s != f32::NEG_INFINITY;

        // Finalize eos among the top `beam` candidates.
        for cand in flat.iter().take(beam) {
            if !is_eos(cand) {
                continue;
            }
            let (score, i) = *cand;
            let mut tokens = prefixes[i / vocab][1..].to_vec();
            tokens.push(sp.eos);
            finalized.push(Hypo {
                tokens,
                score: score / (step_idx + 1) as f32,
            });
            if finalized.len() == beam {
                break;
            }
        }
        if finalized.len() >= beam {
            break;
        }

        // Continue with the first `beam` non-eos candidates, in order.
        let next: Vec<(usize, Vec<u32>, f32)> = flat
            .iter()
            .filter(|c| !is_eos(c))
            .take(beam)
            .map(|&(score, i)| {
                let mut p = prefixes[i / vocab].clone();
                p.push((i % vocab) as u32);
                (i / vocab, p, score)
            })
            .collect();
        parents = next.iter().map(|n| n.0).collect();
        scores = next.iter().map(|n| n.2).collect();
        prefixes = next.into_iter().map(|n| n.1).collect();
    }

    // Stable sort keeps finalization order for equal scores, like Python's sort.
    finalized.sort_by(|a, b| b.score.total_cmp(&a.score));
    finalized.truncate(beam);
    Ok(finalized)
}

fn cmp_desc(a: &(f32, usize), b: &(f32, usize)) -> std::cmp::Ordering {
    b.0.total_cmp(&a.0).then(a.1.cmp(&b.1))
}

#[cfg(test)]
mod tests {
    use super::*;

    const SP: Specials = Specials { eos: 2, pad: 1 };

    /// Toy model over vocab {0,1,eos=2,3,4}: prefers token 3, then eos.
    fn toy(prefixes: &[Vec<u32>], _parents: &[usize]) -> Result<Vec<f32>> {
        let mut out = Vec::new();
        for p in prefixes {
            let probs: [f32; 5] = if p.len() < 3 {
                [0.01, 0.01, 0.08, 0.6, 0.3]
            } else {
                [0.01, 0.01, 0.9, 0.04, 0.04]
            };
            out.extend(probs.iter().map(|x| x.ln()));
        }
        Ok(out)
    }

    #[test]
    fn finds_best_sequence_first() {
        let hypos = beam_search(toy, 5, SP, 3).unwrap();
        assert_eq!(hypos.len(), 3);
        assert_eq!(hypos[0].tokens, vec![3, 3, 2]);
        assert!(hypos.windows(2).all(|w| w[0].score >= w[1].score));
    }

    #[test]
    fn never_emits_eos_first_or_pad() {
        for h in beam_search(toy, 5, SP, 4).unwrap() {
            assert!(h.tokens.len() >= 2, "eos at step 0: {h:?}");
            assert!(!h.tokens.contains(&SP.pad));
            assert_eq!(*h.tokens.last().unwrap(), SP.eos);
        }
    }
}
