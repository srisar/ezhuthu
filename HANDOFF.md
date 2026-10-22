# Ezhuthu — Tamil phonetic input for Windows (handoff)

> Working name: **ezhuthu** (எழுத்து). Rename freely.

## 1. What we're building

A small native Windows tray app that lets the user type Tamil phonetically ("natchathiram") anywhere in Windows and get the correct Tamil word (நட்சத்திரம்), replacing the discontinued Google Input Tools.

The Windows built-in Tamil phonetic input uses fixed letter-to-sound rules. It turns "natchathiram" into நாசதிரம், which is wrong. We use a **neural transliteration model (AI4Bharat IndicXlit) running locally**, with an **optional Claude Haiku "refine" pass** on demand.

### Interaction (v1, the "hotkey overlay" approach)

1. The user is in any app, presses a global hotkey (default `Ctrl+Alt+T`, configurable).
2. A small borderless popup appears near the text caret (or the mouse cursor as a fallback).
3. The user types romanized Tamil. The popup shows:
   - the input line (romanized),
   - a live Tamil preview of the whole phrase (top-1 for each word),
   - a candidate list (top 5) for the **current** word.
4. Keys:
   - `Up`/`Down` or `1`–`5` pick a candidate for the current word.
   - `Space` commits the current word with the selected candidate and starts the next word.
   - `Enter` inserts the full Tamil phrase into the original app and closes the popup.
   - `Esc` cancels.
   - `Ctrl+R` sends the phrase to Claude Haiku for a context-aware fix and replaces the preview with the result. The user still presses `Enter` to insert.
5. Text is typed into the target app with `SendInput` + `KEYEVENTF_UNICODE` (no clipboard).

**Not in v1:** a real IME through the Text Services Framework (TSF). That may come later if v1 proves the quality.

## 2. Hard constraints

- **Rust**, native Win32. **No web technologies**: no Tauri, WebView, Electron or HTML UI.
- **No large new toolchains.** Use the existing MSVC Rust toolchain (already installed for Tauri work). Don't require CMake, LLVM, a CUDA build, or a Visual Studio IDE install.
- Small footprint: a single `.exe`, a model file, and a config file. No ONNX Runtime DLL unless `tract` proves insufficient (see §5).
- Windows 10/11 x64 only.
- Offline by default. The network is used **only** when the user presses `Ctrl+R`.
- Python is allowed **only** for the one-time model export, managed with `uv`, and never shipped.

## 3. Environment check (Phase 0)

Run and report the output before writing code:

```
rustup show                      # expect default toolchain x86_64-pc-windows-msvc
cargo --version
uv --version
```

Also confirm that "Visual Studio Build Tools" (C++ workload and Windows SDK) is installed, for example by checking that `link.exe` resolves during a `cargo build` of a hello-world project. If something is missing, **stop and tell the user** rather than installing toolchains.

**Result (2026-10-22):** passed, nothing installed.

| Item | Found |
|---|---|
| Rust toolchain | `stable-x86_64-pc-windows-msvc` (default), cargo 1.98.1 |
| uv | 0.13.1; Python 3.10.19 already present as a uv-managed interpreter |
| MSVC | Visual Studio Community 2022 (17.14) at `E:\__ProgramFiles\VisualStudio\VisualStudioCommunity`, with the `VC.Tools.x86.x64` component |
| Windows SDK | 10.0.26100 at `E:\Windows Kits\10\` (non-default drive) |
| Linker | hello-world `cargo build` linked and ran |
| Other | git 2.55, gh 2.102 (logged in), git-lfs 3.7.1 (not used, see §12) |

## 4. Architecture

```
┌────────────────────────── ezhuthu.exe ──────────────────────────┐
│ Main thread: Win32 message loop                                 │
│   • tray icon (Shell_NotifyIconW): Settings / Quit             │
│   • RegisterHotKey → WM_HOTKEY                                  │
│   • popup window: EDIT (input) + STATIC (preview) + LISTBOX     │
│     font: "Nirmala UI" (ships with Windows; Tamil shaping       │
│     handled by native controls via Uniscribe/DirectWrite)       │
│                                                                 │
│ xlit module: tract ONNX encoder/decoder + beam search (Rust)    │
│   • word → top-k Tamil candidates (+ optional rescoring)        │
│                                                                 │
│ refine module (worker thread): ureq → Anthropic Messages API    │
│   • result posted back to the UI thread via PostMessageW        │
│                                                                 │
│ insert module: remember target HWND → SetForegroundWindow →     │
│   SendInput(KEYEVENTF_UNICODE) for each UTF-16 unit              │
└─────────────────────────────────────────────────────────────────┘
assets/: xlit_encoder.onnx, xlit_decoder.onnx, vocab_src.json,
         vocab_tgt.json, (optional) ta_word_freq.bin
%APPDATA%\ezhuthu\config.toml: hotkey, model path, API key source, beam size
```

### Crates (keep the list short)

| Purpose | Crate | Notes |
|---|---|---|
| Win32 | `windows` (Microsoft) | Enable only the features needed: `Win32_UI_WindowsAndMessaging`, `Win32_UI_Input_KeyboardAndMouse`, `Win32_UI_Shell`, `Win32_Graphics_Gdi`, `Win32_Foundation`, `Win32_System_LibraryLoader`, `Win32_UI_Controls`, `Win32_System_Threading` |
| ONNX inference | `tract-onnx` | Pure Rust, no DLL. Fallback: `ort` (see §5) |
| HTTP (refine) | `ureq` | Blocking, small. No tokio |
| Config | `serde`, `toml` | |
| JSON | `serde_json` | Vocab files and the API |
| Errors | `anyhow` | |

Use **no async runtime and no GUI framework.** egui, iced and similar libraries do their own text rendering and draw Tamil vowel signs and conjuncts incorrectly. Native controls draw them correctly.

Build with `windows_subsystem = "windows"` for release, keeping the console in debug. Release profile: `opt-level = 3`, `lto = true`, `codegen-units = 1`, `strip = true`, `panic = "abort"`.

## 5. The model: IndicXlit (riskiest part, do it first)

- Repo: https://github.com/AI4Bharat/IndicXlit
- A Roman→Indic multilingual transformer, about 11M parameters, 6 encoder + 6 decoder layers, trained with **fairseq**.
- Checkpoint: `https://github.com/AI4Bharat/IndicXlit/releases/download/v1.0/indicxlit-en-indic-v1.0.zip`, which contains the model and the joint vocabulary/dictionary files.
- The model works on **single words**, with **character-level** input. The target language is selected with a language tag token. **Confirm the exact input format** (character separation, tag token such as `__ta__`, and where it goes) from the repo's inference code or the `ai4bharat-transliteration` pip package source. Don't guess.
- The official inference can **rescore beam outputs with a word-probability dictionary**. Check whether a Tamil word-frequency file is published. If it is, port the rescoring, because it's a large part of why the output picks real words.
- **Check the license** of the code and the weights, and record it in `README.md`.

### Confirmed from source (2026-10-22)

These were checked against `app/ai4bharat/transliteration/transformer/base_engine.py` in the IndicXlit repo, the checkpoint's stored config, and a run of real fairseq 0.12.2.

- **Checkpoint:** fairseq `transformer` (11.49M params) trained with task `translation_multi_simple_epoch`. Pre-norm encoder and decoder, GELU (erf), sinusoidal positions, embeddings scaled by √256, `layernorm_embedding`, final `layer_norm`. The decoder output projection is separate (not shared). Embed dim 256, FFN 1024, 4 heads.
- **Language tags:** come from `lang_list.txt` in the repo, not the zip. It has 22 entries in this order: `en as bn brx gom gu hi kn ks mai ml mni mr ne or pa sa sd si ta te ur`. fairseq appends `__xx__` tokens to every dictionary in that order.
  - Source vocab: 54 tokens (`<s> <pad> </s> <unk>`, `a`–`z`, 2 madeupwords, 22 tags). **`__ta__` = 51.**
  - Target vocab: 806 tokens (4 specials, 780 characters, 22 tags). All Indic `dict.*.txt` files are identical.
- **Input:** lowercase, keep the last run of `[a-z]`, then build `[__ta__, c1, …, cn, </s>]`. The tag is a source-side **prefix**. There is no BOS.
- **Decoder:** starts with `</s>` (id 2) only (`decoder_langtok=False`). Output characters are joined with no separator.
- **Generation:** the official engine uses beam 4 and nbest 4. Otherwise fairseq defaults apply: lenpen 1, normalized scores (`score = Σ logp / len`, eos included), `max_len_b` 200, min length 1. **We use beam 5** because the popup shows five candidates.
- **Rescoring:** `final = 0.9 · exp(score_i)/Σexp(score) + 0.1 · p_dict(w_i)/Σ_in-dict p_dict`. Words missing from the dictionary score 0, and ties keep beam order.
  - The Tamil dictionary is `word_prob_dicts/ta_word_prob_dict.json` inside `https://github.com/AI4Bharat/IndicXlit/releases/download/v1.0/word_prob_dicts.zip` (850 MB zip). The JSON alone is 1.04 GB uncompressed, so it **must be pruned** into a compact `ta_word_freq.bin` before shipping (still to do).
  - Without rescoring, the raw model already gets 9/9 test words top-1.
- **ZWNJ:** the official engine appends ZWNJ (U+200C) after a word-final virama for every language, Tamil included, because of a condition that is always true. **We don't add it.** Tamil doesn't need it, and an invisible character would leak into the user's documents.
- **License:** MIT for both code and models. The README says "The IndicXlit code (and models) are released under the MIT License." No separate license is stated for the word-probability dictionaries.
- **ONNX graphs as exported (they differ from the plan in step 3):**
  - Batch-first, with no padding mask, because we only ever encode one word.
  - The decoder returns **`lprobs[B, V]`**: the log-softmax of the **last** position only, not `logits[B, T, V]`.
  - `encoder_out` stays `[1, S, D]` and is broadcast across the beam inside the decoder.
  - Opset 17, exported with the TorchScript exporter (`dynamo=False`).
- **Parity:**
  - `xlit_ref.py` (a minimal PyTorch port with a port of fairseq's beam search) matches real fairseq 0.12.2 exactly (top-5 and scores) on all test words.
  - ONNX (onnxruntime) matches the PyTorch top-5 on all test words, with a maximum log-prob difference of 4.6e-5.
  - Speed: 84 ms/word on average with onnxruntime in Python (1 thread, beam 5, no KV cache), which is **above the 30 ms target**. Phase 2 needs to measure tract in release mode. If it's too slow, the likely fixes are precomputing the cross-attention K/V or adding a KV cache.

### Known pitfalls

- **fairseq doesn't work on Python 3.11+** because of its mutable dataclass defaults. Use **Python 3.10** via `uv venv --python 3.10`.
- A community CTranslate2 conversion exists (`Singla0009/all-indic-transliteration` on Hugging Face), which shows the patches needed to load the checkpoint. It's useful as a reference. **Do not use CTranslate2 at runtime**, because it's a C++ build with CMake.

### Export plan (in `tools/export/`, Python 3.10, uv-managed)

1. Load the checkpoint, either with fairseq or, if fairseq won't install, with **plain `torch.load`** plus a minimal re-implementation of the fairseq transformer in PyTorch (preferred fallback: only a few layers, and no fairseq dependency).
2. Write a reference `transliterate(word) -> top-k` in Python, and check it against the official output on the test words (§8).
3. Export **two ONNX graphs**:
   - `xlit_encoder.onnx`: `src_tokens[1, S]` → `encoder_out`, plus the padding mask if needed.
   - `xlit_decoder.onnx`: `(prev_output_tokens[B, T], encoder_out)` → `logits[B, T, V]`.
   - **No KV cache** in v1. Words are under about 30 characters, so re-running the decoder on the full prefix is cheap. Use dynamic axes for B, S and T. Use an opset version that `tract` supports (start at 14–17).
4. Dump the vocabularies to `vocab_src.json` and `vocab_tgt.json`, including the special tokens (bos, eos, pad, unk, language tags).
5. Run a parity script: the ONNX output (via `onnxruntime` in Python) must match the PyTorch top-5 on all test words.
6. Optional: int8 dynamic quantization. Keep it **only if** `tract` runs it and accuracy is unchanged.

### Fallback order if tract fails

1. Try a different opset, or simplify the graph with `onnxsim`.
2. Switch to the `ort` crate. This ships `onnxruntime.dll`, about 10–20 MB, which is acceptable.
3. Last resort: convert the weights to safetensors and implement the transformer directly in `candle`, which is pure Rust.

## 6. Win32 details

- **Hotkey:** use `RegisterHotKey(NULL, id, MOD_CONTROL|MOD_ALT|MOD_NOREPEAT, 'T')`. If registration fails because the combination is taken, show a tray balloon and keep running.
- **Target window:** on `WM_HOTKEY`, store `GetForegroundWindow()` before showing the popup.
- **Caret position:** call `GetGUIThreadInfo(GetWindowThreadProcessId(target))` and use `rcCaret` with `hwndCaret`, mapped to screen coordinates via `ClientToScreen`. If it's empty (Chromium/Electron apps often don't report a caret), fall back to `GetCursorPos`. Keep the popup on screen using the monitor's work area (`MonitorFromPoint` + `GetMonitorInfoW`). UI Automation caret lookup is a Phase 5 item.
- **Popup window:** `WS_POPUP`, with `WS_EX_TOPMOST | WS_EX_TOOLWINDOW` (so it doesn't appear in the taskbar). Hide it when it loses focus (`WM_ACTIVATE` with `WA_INACTIVE`) and treat that as cancel.
- **Live update:** subclass the EDIT control. On each change, re-run transliteration **only for the current word** and cache results per word.
- **Insert:** hide the popup, call `SetForegroundWindow(target)` (this generally works because we were just foreground after `WM_HOTKEY`), wait briefly (about 30–50 ms or until the target is foreground), then send the text as one `SendInput` batch: a key-down and key-up pair with `KEYEVENTF_UNICODE` for each UTF-16 code unit.
- **Elevated windows:** UIPI blocks input into windows running as administrator. Detect this and show a tray notice. Don't try to work around it.
- **Paste fallback:** some apps mishandle `KEYEVENTF_UNICODE`. Add a config option `insert_mode = "unicode" | "clipboard"`. The clipboard mode saves the clipboard, sets the text, sends `Ctrl+V`, and restores the clipboard.
- **DPI:** declare per-monitor v2 DPI awareness in the manifest (`embed-resource` or a `build.rs` manifest) and scale the popup accordingly.

## 7. Claude refine (`Ctrl+R`)

- Endpoint: Anthropic Messages API. Model: **`claude-haiku-5-5`**. Read the model name from config so it can be changed.
- API key: read the `ANTHROPIC_API_KEY` environment variable, or `api_key` in `config.toml`. **Never log it.** If there's no key, `Ctrl+R` shows "refine not configured" in the popup.
- Send the **romanized input and the current Tamil preview**. Ask for the corrected Tamil text **only**, with no explanation and no quotes, preserving the user's intended words, with no translation into other words and no added content. Use a low `max_tokens` (about 200) and a 5-second timeout.
- Run the request on a worker thread and `PostMessageW` the result back to the UI thread. Show a small "refining…" indicator. If it fails, keep the local result and show the error briefly.
- Refine is never sent automatically. The network is used only on an explicit `Ctrl+R`.

## 8. Test words (acceptance)

Each expected word must appear in the **top 5**, and most should be **top 1**:

| Input | Expected |
|---|---|
| natchathiram | நட்சத்திரம் |
| vanakkam | வணக்கம் |
| thamizh | தமிழ் |
| nandri | நன்றி |
| amma | அம்மா |
| kadal | கடல் |
| paadal | பாடல் |
| ilangai | இலங்கை |
| mattakkalappu | மட்டக்களப்பு |

Put these in `tests/words.tsv`, and use them both for the Python parity script and for a Rust integration test (`cargo test`). Record top-1 accuracy and the time per word.

## 9. Phases and done criteria

| Phase | Deliverable | Done when |
|---|---|---|
| 0 | Environment check (§3) | Toolchain confirmed; nothing new installed without the user's approval |
| 1 | `tools/export/`: Python 3.10 export and parity | ONNX matches PyTorch top-5 on every test word |
| 2 | `ezhuthu-xlit` library crate and an `xlit-cli` binary (`xlit-cli natchathiram` prints top 5) | Test words pass; **under 30 ms per word** on CPU (release build) |
| 3 | Tray app: hotkey, popup, live candidates, insertion | Works in Notepad, Word, Chrome (text box), VS Code and the Windows search box |
| 4 | `Ctrl+R` refine | Works with a key, degrades cleanly without one or offline |
| 5 | Polish | See the backlog below |

**Phase 5 backlog:** a "convert selection" hotkey (copy the selected romanized text, transliterate it, replace it); remembering the user's candidate picks (a small local user dictionary that boosts them next time); an autostart toggle (HKCU `Run` registry key); UI Automation caret position; a settings dialog; an installer (a zip is fine to begin with).

## 10. Repository layout

```
ezhuthu/
├─ Cargo.toml              # workspace
├─ crates/
│  ├─ xlit/                # model loading, tokenization, beam search, rescoring
│  └─ app/                 # Win32 tray app (bin: ezhuthu)
├─ tools/export/           # uv project, Python 3.10, export and parity scripts
├─ assets/                 # model + vocab (git-lfs or .gitignore + download script)
├─ tests/words.tsv
├─ README.md               # build steps, model source + license
└─ HANDOFF.md              # this file
```

Don't commit large model files directly. Use git-lfs or a `tools/fetch-assets` script plus `.gitignore`. Ask the user which they prefer.

## 11. Working agreements

- Conventional Commits; work on feature branches; no direct commits to `main`.
- Ask before destructive operations (deleting files, force-pushing, rewriting history).
- Ask before installing any new system-level tool. `cargo add` and `uv add` inside the project are fine.
- Rust code style: run `cargo fmt` and `cargo clippy -- -D warnings`. Keep `unsafe` confined to small, commented Win32 wrapper functions.
- When something in §5 doesn't match reality (input format, vocabulary, rescoring files), **check the source code and update this document** rather than guessing.

## 12. Open questions for the user

Answered on 2026-10-22:

1. Is the hotkey `Ctrl+Alt+T` OK? **Yes**, as the default. It stays configurable.
2. Model assets: git-lfs or a download script? **A download script.** `assets/` is gitignored, and `tools/fetch-assets` downloads the exported ONNX and vocab files from a GitHub Release.
3. Is the name "ezhuthu" OK? **Yes.**
4. (Added) Repo hosting: a **private GitHub repo** created with `gh repo create`. The only commit on `main` is a bootstrap commit (this file and `.gitignore`). Everything after that goes through feature branches and PRs.
