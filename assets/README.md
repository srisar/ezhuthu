# assets/

Model files live here but are never committed. Get them with:

```bash
tools/fetch-assets.sh
```

This downloads the `assets-v1` GitHub Release and checks it against `tools/assets.sha256`. To rebuild them from the upstream checkpoint instead, see "Model export" in the top-level README.

| File | Size | From |
|---|---|---|
| `xlit_encoder.onnx` | 23 MB | `tools/export/export_onnx.py`: encoder plus cross-attention K/V |
| `xlit_decoder.onnx` | 25 MB | `tools/export/export_onnx.py`: one decoder step with KV cache |
| `vocab_src.json`, `vocab_tgt.json` | 10 KB | `tools/export/export_onnx.py` |
| `ta_word_freq.bin` | 10 MB | `tools/export/word_freq.py`: the 500k most frequent Tamil words, used for rescoring |

All of these are derived from AI4Bharat IndicXlit (MIT license).
