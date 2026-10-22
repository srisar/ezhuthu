# ezhuthu (எழுத்து)

Tamil phonetic input for Windows. Type `natchathiram`, get நட்சத்திரம். It is a small native Win32 tray app (Rust) that runs the AI4Bharat IndicXlit transliteration model locally, with an optional on-demand refine pass via Claude.

The design, constraints and phase plan are in [HANDOFF.md](HANDOFF.md).

## Status

| Phase | State |
|---|---|
| 0: environment check | done |
| 1: model export and parity (`tools/export/`) | done: ONNX matches PyTorch, which matches fairseq, on all test words |
| 2: `xlit` crate and `xlit-cli` (`crates/xlit`) | done: tract, KV-cached decoder, exact top-5 parity with Python |
| 3: tray app (`crates/app`) | built; manual testing in target apps pending |
| 4–5: refine, polish | not started |

## The app

```bash
tools/fetch-assets.sh
cargo build --release -p ezhuthu
./target/release/ezhuthu.exe
```

`ezhuthu.exe` lives in the tray. In any app, press **Ctrl+Alt+T**, type Tamil in English letters, then:

| Key | Action |
|---|---|
| ↑ / ↓, or 1–5 | choose a candidate (a digit picks it and moves to the next word) |
| Space | keep the chosen candidate and start the next word |
| Backspace / Ctrl+Backspace | delete a letter / a word |
| Ctrl+V | paste text into the popup |
| Enter | type the Tamil phrase into the app |
| Esc | cancel |

The popup never takes focus, so it also works over the Start/search box. Settings are in `%APPDATA%\ezhuthu\config.toml`; use "Open settings file" in the tray menu. It looks for model files in `assets/` next to the exe, or in the workspace `assets/` when run from a cargo build.

## Transliteration library and CLI (Rust)

`crates/xlit` (`ezhuthu-xlit`) is pure Rust: ONNX runs on [tract](https://github.com/sonos/tract), with no DLLs. It needs the exported assets in `assets/` (see below).

```bash
cargo run --release --bin xlit-cli -- natchathiram vanakkam
cargo run --release --bin xlit-cli -- --bench tests/words.tsv
cargo test --release -p ezhuthu-xlit
```

The tests check the §8 acceptance words (each expected word in the top 5) and exact top-5 parity with the Python reference on `tests/golden_top5.tsv`. To regenerate that file after re-exporting, run `uv run python xlit_ref.py --golden ../../tests/parity_words.txt > ../../tests/golden_top5.tsv` from `tools/export`.

The release profile uses LTO with `codegen-units = 1`, so the first release build takes several minutes.

## Model export (one-time, Python)

Python is used only to produce the ONNX assets and is never shipped. You need [uv](https://docs.astral.sh/uv/). Python 3.10 is pinned in `tools/export/.python-version`.

```bash
cd tools/export

# 1. Get the checkpoint (127 MB) into the gitignored work/ directory
mkdir -p work/download
curl -L -o work/download/indicxlit-en-indic-v1.0.zip \
  https://github.com/AI4Bharat/IndicXlit/releases/download/v1.0/indicxlit-en-indic-v1.0.zip
unzip -o work/download/indicxlit-en-indic-v1.0.zip -d work/model

# 2. Reference transliteration (PyTorch)
uv run python xlit_ref.py natchathiram vanakkam

# 3. Export to ../../assets/ and check ONNX against PyTorch
uv run python export_onnx.py
uv run python parity.py
```

### Verifying against fairseq (optional)

`fairseq_check.py` runs the checkpoint through real fairseq 0.12.2 so you can diff its output against `xlit_ref.py`. fairseq's C++ extensions don't build here, but generation doesn't need them, so it runs from the unpacked sdist in a throwaway venv:

```bash
cd tools/export/work
uv venv --python 3.10 fairseq-venv
VIRTUAL_ENV="$PWD/fairseq-venv" uv pip install --extra-index-url https://download.pytorch.org/whl/cpu \
  --index-strategy unsafe-best-match "torch==2.4.1+cpu" "numpy<2" "hydra-core==1.0.7" "omegaconf==2.0.6" \
  bitarray sacrebleu regex tqdm cffi
curl -L https://files.pythonhosted.org/packages/source/f/fairseq/fairseq-0.12.2.tar.gz | tar xz
printf '%s\n' en as bn brx gom gu hi kn ks mai ml mni mr ne or pa sa sd si ta te ur > model/lang_list.txt
cd ..
PYTHONPATH="$PWD/work/fairseq-0.12.2" work/fairseq-venv/Scripts/python.exe \
  fairseq_check.py work/model 5 $(cut -f1 ../../tests/words.tsv) > work/fairseq_out.txt
uv run python xlit_ref.py $(cut -f1 ../../tests/words.tsv) > work/ref_out.txt
diff work/fairseq_out.txt work/ref_out.txt && echo identical
```

## Assets

Model files are never committed. `assets/` is gitignored. To get the files, run:

```bash
tools/fetch-assets.sh
```

This downloads the `assets-v1` GitHub Release (about 58 MB) with `gh` and verifies it against `tools/assets.sha256`. You need `gh auth login` while the repository is private. [assets/README.md](assets/README.md) lists the files.

To rebuild the word-frequency dictionary from upstream, run these from `tools/export`. `fetch` downloads only the 105 MB Tamil entry of the 850 MB zip.

```bash
uv run python word_freq.py fetch
uv run python word_freq.py build --top 500000
```

If you rebuild any asset, update `tools/assets.sha256` and publish a new release tag.

## Model source and license

- Model: [AI4Bharat IndicXlit](https://github.com/AI4Bharat/IndicXlit), checkpoint `indicxlit-en-indic-v1.0`. Madhani et al., *Aksharantar: Open Indic-language Transliteration datasets and models for the Next Billion Users* (2022).
- License: **MIT**, covering both code and models ("The IndicXlit code (and models) are released under the MIT License"; LICENSE: Copyright (c) 2022 AI4Bhārat).
