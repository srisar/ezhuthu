//! How well whole-word prediction works: types each word of
//! `tests/predict_words.txt` letter by letter and checks whether the word
//! the model gives for the full spelling is already among the top
//! predictions after 3, 4, 5… letters.
//!
//!     cargo run --release -p ezhuthu-xlit --example predict_eval [assets] [words]

use std::path::PathBuf;
use std::time::{Duration, Instant};

use anyhow::Result;
use ezhuthu_xlit::Transliterator;

/// Predictions shown in the popup.
const SHOWN: usize = 3;
/// A dictionary completion is shown only when it makes up this share of the
/// words starting the same way (as in the app).
const MIN_SHARE: f32 = 0.5;
/// Prefix lengths reported.
const LENGTHS: [usize; 6] = [3, 4, 5, 6, 7, 8];

fn main() -> Result<()> {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let mut args = std::env::args().skip(1);
    let assets = args
        .next()
        .map(PathBuf::from)
        .unwrap_or(root.join("assets"));
    let words_path = args
        .next()
        .map(PathBuf::from)
        .unwrap_or(root.join("tests/predict_words.txt"));
    let xlit = Transliterator::load(&assets)?;
    anyhow::ensure!(
        xlit.has_word_freq(),
        "ta_word_freq.bin not found in {}",
        assets.display()
    );
    let words: Vec<String> = std::fs::read_to_string(&words_path)?
        .lines()
        .map(|l| l.trim().to_ascii_lowercase())
        .filter(|w| w.len() >= 4)
        .collect();

    // Per prefix length: (words long enough to have a prefix that long, hits).
    let mut by_len = [(0usize, 0usize); LENGTHS.len()];
    let mut first_hit = Vec::new(); // letters typed before the first hit
    let mut predict_time = Duration::ZERO;
    let mut predict_calls = 0u32;
    let mut misses = Vec::new();
    // The single confident completion (what the popup shows): how often one
    // is offered from 3 letters on, and how often it is the right word.
    let (mut prefixes, mut offered, mut right) = (0usize, 0usize, 0usize);
    for word in &words {
        let Some(target) = xlit.transliterate(word)?.into_iter().next().map(|c| c.text) else {
            continue;
        };
        let mut hit_at = None;
        for len in 3..word.len() {
            let prefix = &word[..len];
            let cands = xlit.transliterate(prefix)?;
            let start = Instant::now();
            let predicted = xlit.predict(&cands, SHOWN);
            prefixes += 1;
            if let Some(word) = xlit.complete(&cands, MIN_SHARE) {
                offered += 1;
                right += (word == target) as usize;
            }
            predict_time += start.elapsed();
            predict_calls += 1;
            // Typing on would also show the target among the candidates.
            let hit =
                predicted.contains(&target) || cands.first().is_some_and(|c| c.text == target);
            if let Some(i) = LENGTHS.iter().position(|&l| l == len) {
                by_len[i].0 += 1;
                by_len[i].1 += hit as usize;
            }
            if hit && hit_at.is_none() {
                hit_at = Some(len);
            }
        }
        match hit_at {
            Some(len) => first_hit.push((len, word.len())),
            None => misses.push(format!("{word} → {target}")),
        }
    }

    println!(
        "{} words, top {SHOWN} predictions (or the first candidate)\n",
        words.len()
    );
    println!("letters typed   words   target shown");
    for (len, (n, hits)) in LENGTHS.iter().zip(by_len) {
        if n > 0 {
            println!(
                "{len:>14}   {n:>5}   {:>5.1} %",
                100.0 * hits as f64 / n as f64
            );
        }
    }
    let typed: usize = first_hit.iter().map(|(at, _)| at).sum();
    let full: usize = first_hit.iter().map(|(_, len)| len).sum();
    println!(
        "\npredicted before the last letter: {} of {} words",
        first_hit.len(),
        words.len()
    );
    if full > 0 {
        println!(
            "letters typed for those words: {typed} of {full} ({:.0} % saved)",
            100.0 * (full - typed) as f64 / full as f64
        );
    }
    println!(
        "prediction time: {:.2} ms on average ({predict_calls} calls, dictionary lookups only)",
        predict_time.as_secs_f64() * 1000.0 / predict_calls.max(1) as f64
    );
    println!(
        "
confident completion (share >= {MIN_SHARE}): offered at {offered} of {prefixes} prefixes ({:.0} %), right {right} times ({:.0} % of those offered)",
        100.0 * offered as f64 / prefixes.max(1) as f64,
        100.0 * right as f64 / offered.max(1) as f64
    );
    if !misses.is_empty() {
        println!("\nnever predicted ({}):", misses.len());
        for m in misses.iter().take(40) {
            println!("  {m}");
        }
    }
    Ok(())
}
