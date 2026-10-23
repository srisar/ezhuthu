# ezhuthu (எழுத்து)

Tamil phonetic input for Windows. Type `natchathiram`, get நட்சத்திரம். It is a small native Win32 tray app (Rust) that runs the AI4Bharat IndicXlit transliteration model locally, with an optional on-demand refine pass via Claude.

The design, constraints and phase plan are in [HANDOFF.md](HANDOFF.md).

## Status

| Phase | State |
|---|---|
| 0: environment check | done |
| 1: model export and parity (`tools/export/`) | done: ONNX matches PyTorch, which matches fairseq, on all test words |
| 2: `xlit` crate and `xlit-cli` (`crates/xlit`) | done: tract, KV-cached decoder, exact top-5 parity with Python |
| 3: tray app (`crates/app`) | done: tested in Notepad, Chrome, Word, VS Code; Windows search box not yet tried |
| 4: refine (Ctrl+R) | deferred |
| 5: polish | per-user installer, Settings window, Start with Windows done |

## Install

Run `ezhuthu-setup-<version>.exe`. It installs for the current user only, with no administrator rights. The setup window shows the install folder, by default `%LOCALAPPDATA%\Programs\ezhuthu`. Use **Browse…** to pick another location (an `ezhuthu` folder is created there) or type a path. Setup then:
- restricts the install folder so only you, SYSTEM and Administrators can change it. A drive without permissions (FAT32/exFAT) is refused;
- adds a Start Menu shortcut;
- adds an entry in **Settings > Apps**, whose uninstall runs `ezhuthu.exe --uninstall`;
- sets **Start with Windows** if its checkbox is ticked (you can change it later in **Settings**);
- starts the app.

Running a newer setup updates an existing install in place, and settings in `%APPDATA%\ezhuthu` are kept. Uninstall deletes only ezhuthu's own files (a fixed list built into the program, after checking for links), so anything else in the folder is left alone. Unattended install: `ezhuthu-setup.exe --silent [--dir <folder>] [--autostart] [--no-launch]`. The setup isn't code-signed yet, so Windows SmartScreen may warn the first time. Choose **More info > Run anyway**.

To build the installer (after `tools/fetch-assets.sh`):

```bash
tools/build-installer.sh   # -> dist/ezhuthu-setup-<version>.exe
```

It is a single self-contained file: the release `ezhuthu.exe` plus the model assets.

## The app

```bash
tools/fetch-assets.sh
cargo build --release -p ezhuthu
./target/release/ezhuthu.exe
```

`ezhuthu.exe` lives in the tray. **Ctrl+Alt+T** turns Tamil mode on or off. The tray icon is blue when it's on and grey when it's off. While it's on, type Tamil in English letters in any app, a word at a time. A popup at the caret shows the candidates:

| Key (while a word is in progress) | Action |
|---|---|
| Space | insert the chosen Tamil word and a space |
| Enter | insert the chosen word without a space |
| 1–5 | insert that candidate and a space |
| ↑ / ↓ | choose a candidate |
| Backspace / Ctrl+Backspace | delete a letter / drop the word |
| Esc | drop the word |
| any other key (Tab, arrows, Ctrl+S…) | insert the word, then the key does its usual job |

When no word is in progress, digits, punctuation, Enter, arrows and shortcuts type normally. Punctuation typed right after a word stays with it, so `amma,` gives அம்மா,. The popup never takes focus, so it also works over the Start/search box.

Change settings from the tray menu: **Settings...** has two tabs. **General** sets the Tamil-mode shortcut (one of four presets, or any custom combination), suggestions per word, how text is inserted (typing, or pasting for apps that garble typed Tamil) and Start with Windows. **About** shows the version and credits for the model and libraries. Launching ezhuthu again while it is running also opens Settings. They are saved in `%APPDATA%\ezhuthu\config.toml`. The app looks for model files in `assets/` next to the exe. Dev builds (`dev` and `fast` profiles) also look in the workspace `assets/`.

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

## Icons

The app and tray icons are committed in `crates/app/icons/`, and the build scripts link them into `ezhuthu.exe` and `ezhuthu-setup.exe` without a resource compiler. The mark is எ followed by a text cursor on a dark tile; the "off" tray icon is the letter on grey. `ezhuthu.svg` is the vector master. To redraw them, run this from `tools/icons` (the font is only needed here, never shipped):

```bash
curl -L -o "work/AnekTamil[wdth,wght].ttf" "https://github.com/google/fonts/raw/main/ofl/anektamil/AnekTamil%5Bwdth,wght%5D.ttf"
uv run make_icons.py "work/AnekTamil[wdth,wght].ttf" ../../crates/app/icons --preview work/preview.png
```

The letter's outline comes from Anek Tamil ExtraBold (SIL Open Font License 1.1).

## Model source and license

- Model: [AI4Bharat IndicXlit](https://github.com/AI4Bharat/IndicXlit), checkpoint `indicxlit-en-indic-v1.0`. Madhani et al., *Aksharantar: Open Indic-language Transliteration datasets and models for the Next Billion Users* (2022).
- License: **MIT**, covering both code and models ("The IndicXlit code (and models) are released under the MIT License"; LICENSE: Copyright (c) 2022 AI4Bhārat).
