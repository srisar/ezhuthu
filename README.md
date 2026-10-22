# ezhuthu (எழுத்து)

Tamil phonetic input for Windows. Type `natchathiram`, get நட்சத்திரம். It is a small native Win32 tray app (Rust) that runs the AI4Bharat IndicXlit transliteration model locally, with an optional on-demand refine pass via Claude.

The design, constraints and phase plan are in [HANDOFF.md](HANDOFF.md).

## Status

| Phase | State |
|---|---|
| 0: environment check | done |
| 1: model export and parity (`tools/export/`) | done: ONNX matches PyTorch, which matches fairseq, on all test words |
| 2: `xlit` crate and `xlit-cli` (`crates/xlit`) | done: tract, KV-cached decoder, exact top-5 parity with Python |
| 3–5: tray app, refine, polish | not started |

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

Model files are never committed. `assets/` is gitignored, and a `tools/fetch-assets` script will download the exported files from a GitHub Release (coming in a later phase).

## Model source and license

- Model: [AI4Bharat IndicXlit](https://github.com/AI4Bharat/IndicXlit), checkpoint `indicxlit-en-indic-v1.0`. Madhani et al., *Aksharantar: Open Indic-language Transliteration datasets and models for the Next Billion Users* (2022).
- License: **MIT**, covering both code and models ("The IndicXlit code (and models) are released under the MIT License"; LICENSE: Copyright (c) 2022 AI4Bhārat).
