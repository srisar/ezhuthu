//! Acceptance (HANDOFF §8) and parity with the Python reference.
//! Needs the exported assets in `<workspace>/assets` (or `EZHUTHU_ASSETS`).

use std::path::PathBuf;
use std::sync::OnceLock;
use std::time::Instant;

use ezhuthu_xlit::Transliterator;

fn workspace() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn xlit() -> &'static Transliterator {
    static XLIT: OnceLock<Transliterator> = OnceLock::new();
    XLIT.get_or_init(|| {
        let assets = std::env::var_os("EZHUTHU_ASSETS")
            .map_or_else(|| workspace().join("assets"), PathBuf::from);
        Transliterator::load(&assets).unwrap_or_else(|e| {
            panic!(
                "loading assets from {}: {e:#}\n\
                 run tools/export/export_onnx.py (see README.md) first",
                assets.display()
            )
        })
    })
}

fn tsv(name: &str) -> Vec<(String, String)> {
    std::fs::read_to_string(workspace().join("tests").join(name))
        .unwrap()
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| {
            let (a, b) = l.split_once('\t').expect("tab-separated line");
            (a.to_string(), b.to_string())
        })
        .collect()
}

#[test]
fn acceptance_words_in_top5() {
    let rows = tsv("words.tsv");
    let mut top1 = 0;
    let start = Instant::now();
    for (word, expected) in &rows {
        let cands: Vec<String> = xlit()
            .transliterate(word)
            .unwrap()
            .into_iter()
            .map(|c| c.text)
            .collect();
        let rank = cands.iter().position(|c| c == expected);
        assert!(
            rank.is_some(),
            "{word}: expected {expected} in top 5, got {cands:?}"
        );
        top1 += (rank == Some(0)) as usize;
    }
    let per_word = start.elapsed().as_secs_f64() * 1000.0 / rows.len() as f64;
    println!(
        "top-1 {top1}/{}, {per_word:.1} ms/word (this build profile)",
        rows.len()
    );
    assert!(top1 * 2 > rows.len(), "only {top1}/{} top-1", rows.len());
}

#[test]
fn top5_matches_python_reference() {
    let mut mismatches = Vec::new();
    for (word, golden) in tsv("golden_top5.tsv") {
        let got: Vec<String> = xlit()
            .transliterate(&word)
            .unwrap()
            .into_iter()
            .map(|c| c.text)
            .collect();
        let want: Vec<&str> = golden.split('|').collect();
        if got != want {
            mismatches.push(format!("{word}:\n  rust:   {got:?}\n  python: {want:?}"));
        }
    }
    assert!(mismatches.is_empty(), "{}", mismatches.join("\n"));
}

#[test]
fn rejects_input_without_latin_letters() {
    assert!(xlit().transliterate("123 !?").is_err());
    assert!(xlit().transliterate("தமிழ்").is_err());
}
