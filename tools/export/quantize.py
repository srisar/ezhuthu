"""Experiment (HANDOFF §5 step 6): int8 dynamic quantization of the ONNX graphs.

Writes quantized copies plus the vocab files into ../../assets-int8/ and
compares their beam-search top-5 against the fp32 golden file
(tests/golden_top5.tsv). Kept only if accuracy is unchanged.

Usage: uv run python quantize.py [--per-channel]
"""

from __future__ import annotations

import argparse
import shutil
import sys
from pathlib import Path

import onnxruntime as ort
from onnxruntime.quantization import QuantType, quantize_dynamic

from export_onnx import ASSETS
from parity import OrtCached
from xlit_ref import BEAM, beam_search, decode_tokens, encode_source, load_dicts

ROOT = Path(__file__).resolve().parents[2]
OUT = ROOT / "assets-int8"


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--per-channel", action="store_true")
    args = ap.parse_args()

    OUT.mkdir(exist_ok=True)
    for name in ("xlit_encoder.onnx", "xlit_decoder.onnx"):
        quantize_dynamic(
            ASSETS / name, OUT / name, weight_type=QuantType.QInt8, per_channel=args.per_channel
        )
        print(f"{name}: {(OUT / name).stat().st_size:,} bytes")
    for name in ("vocab_src.json", "vocab_tgt.json"):
        shutil.copy(ASSETS / name, OUT / name)

    src_dict, tgt = load_dicts()
    opts = ort.SessionOptions()
    opts.intra_op_num_threads = 1
    enc = ort.InferenceSession(str(OUT / "xlit_encoder.onnx"), opts)
    dec = ort.InferenceSession(str(OUT / "xlit_decoder.onnx"), opts)

    golden = [l.split("\t") for l in (ROOT / "tests" / "golden_top5.tsv").read_text(encoding="utf-8").splitlines() if l]
    expected = dict(l.split("\t") for l in (ROOT / "tests" / "words.tsv").read_text(encoding="utf-8").splitlines() if l)
    same_list = same_top1 = 0
    for word, gold in golden:
        src = encode_source(word, src_dict)
        hypos = beam_search(OrtCached(enc, dec, src), tgt, len(src), BEAM)
        got = [decode_tokens(h.tokens, tgt) for h in hypos]
        want = gold.split("|")
        same_list += got == want
        same_top1 += got[0] == want[0]
        flag = "" if got == want else "  <- differs"
        if word in expected:
            flag += f"  [expected rank {got.index(expected[word]) + 1 if expected[word] in got else None}]"
        if flag:
            print(f"{word:16s} {'|'.join(got)}{flag}")
    print(f"\nidentical top-5: {same_list}/{len(golden)}, identical top-1: {same_top1}/{len(golden)}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
