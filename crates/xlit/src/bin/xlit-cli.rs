//! `xlit-cli natchathiram` prints the top-5 Tamil candidates.
//!
//! Options: `--assets DIR` (default: $EZHUTHU_ASSETS, ./assets, then the
//! workspace assets/), `--beam N`, `--bench FILE` (time every first-column
//! word in a TSV, e.g. tests/words.tsv).

use std::path::{Path, PathBuf};
use std::time::Instant;

use anyhow::{Context, Result, bail};
use ezhuthu_xlit::{DEFAULT_BEAM, Transliterator};

fn default_assets() -> PathBuf {
    if let Some(dir) = std::env::var_os("EZHUTHU_ASSETS") {
        return dir.into();
    }
    let local = PathBuf::from("assets");
    if local.join("vocab_src.json").exists() {
        return local;
    }
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets")
}

fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let mut assets = None;
    let mut beam = DEFAULT_BEAM;
    let mut bench = None;
    let mut words = Vec::new();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--assets" => assets = Some(PathBuf::from(args.next().context("--assets DIR")?)),
            "--beam" => beam = args.next().context("--beam N")?.parse()?,
            "--bench" => bench = Some(PathBuf::from(args.next().context("--bench FILE")?)),
            "-h" | "--help" => {
                println!("usage: xlit-cli [--assets DIR] [--beam N] [--bench FILE] WORD...");
                return Ok(());
            }
            _ => words.push(arg),
        }
    }
    let assets = assets.unwrap_or_else(default_assets);

    let t0 = Instant::now();
    let xlit = Transliterator::load(&assets)
        .with_context(|| format!("loading assets from {}", assets.display()))?
        .with_beam(beam);
    eprintln!("loaded in {:.0} ms", t0.elapsed().as_secs_f64() * 1000.0);

    if let Some(file) = bench {
        return run_bench(&xlit, &file);
    }
    if words.is_empty() {
        bail!("no words given (try: xlit-cli natchathiram)");
    }
    for word in &words {
        let t = Instant::now();
        let cands = xlit.transliterate(word)?;
        println!("{word}  ({:.1} ms)", t.elapsed().as_secs_f64() * 1000.0);
        for (i, c) in cands.iter().enumerate() {
            println!("  {}. {}\t{:.4}", i + 1, c.text, c.score);
        }
    }
    Ok(())
}

fn run_bench(xlit: &Transliterator, file: &Path) -> Result<()> {
    let text = std::fs::read_to_string(file)?;
    let words: Vec<&str> = text
        .lines()
        .filter_map(|l| l.split('\t').next())
        .filter(|w| !w.trim().is_empty())
        .collect();
    xlit.transliterate(words[0])?; // warm-up
    const ROUNDS: usize = 5;
    let mut times = Vec::new();
    for word in &words {
        let t = Instant::now();
        for _ in 0..ROUNDS {
            xlit.transliterate(word)?;
        }
        let ms = t.elapsed().as_secs_f64() * 1000.0 / ROUNDS as f64;
        println!("{word:20} {ms:7.1} ms");
        times.push(ms);
    }
    let mean = times.iter().sum::<f64>() / times.len() as f64;
    let max = times.iter().cloned().fold(0.0, f64::max);
    println!(
        "mean {mean:.1} ms/word, max {max:.1} ms ({} words)",
        times.len()
    );
    Ok(())
}
