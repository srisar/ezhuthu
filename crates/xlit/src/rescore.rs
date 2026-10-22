//! Word-probability rescoring from the official engine
//! (`base_engine.py::rescore`, alpha = 0.9).

use std::collections::HashMap;

use crate::Candidate;

pub const ALPHA: f64 = 0.9;

/// Unigram probability of a whole Tamil word, if known.
pub trait WordProb {
    fn prob(&self, word: &str) -> Option<f64>;
}

impl WordProb for HashMap<String, f64> {
    fn prob(&self, word: &str) -> Option<f64> {
        self.get(word).copied()
    }
}

/// `alpha * exp(score_i) / Σ exp(score) + (1 - alpha) * p(w_i) / Σ_in-dict p(w)`.
///
/// Words missing from the dictionary get 0, so they sink below every known
/// word; ties keep beam order (stable sort). `rescored` holds the new score.
pub fn rescore(cands: &[Candidate], dict: &dyn WordProb, alpha: f64) -> Vec<(Candidate, f64)> {
    let beam_p: Vec<f64> = cands.iter().map(|c| (c.score as f64).exp()).collect();
    let beam_total: f64 = beam_p.iter().sum();
    let dict_p: Vec<Option<f64>> = cands.iter().map(|c| dict.prob(&c.text)).collect();
    let dict_total: f64 = dict_p.iter().flatten().sum();

    let mut out: Vec<(Candidate, f64)> = cands
        .iter()
        .zip(beam_p.iter().zip(&dict_p))
        .map(|(c, (bp, dp))| {
            let s = match dp {
                Some(p) => alpha * bp / beam_total + (1.0 - alpha) * p / dict_total,
                None => 0.0,
            };
            (c.clone(), s)
        })
        .collect();
    out.sort_by(|a, b| b.1.total_cmp(&a.1));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cand(text: &str, score: f32) -> Candidate {
        Candidate {
            text: text.into(),
            score,
        }
    }

    #[test]
    fn dictionary_word_can_overtake_beam_top() {
        let cands = [cand("a", -0.10), cand("b", -0.12), cand("c", -0.5)];
        let dict: HashMap<String, f64> = [("b".into(), 1e-3), ("a".into(), 1e-6)].into();
        let out = rescore(&cands, &dict, ALPHA);
        let order: Vec<&str> = out.iter().map(|(c, _)| c.text.as_str()).collect();
        assert_eq!(order, ["b", "a", "c"]);
        assert_eq!(out[2].1, 0.0);
    }

    #[test]
    fn unknown_words_keep_beam_order() {
        let cands = [cand("x", -0.1), cand("y", -0.2)];
        let out = rescore(&cands, &HashMap::new(), ALPHA);
        let order: Vec<&str> = out.iter().map(|(c, _)| c.text.as_str()).collect();
        assert_eq!(order, ["x", "y"]);
    }
}
