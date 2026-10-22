"""Export the IndicXlit encoder/decoder to ONNX and dump the vocabularies.

Writes to ../../assets/:
  xlit_encoder.onnx  src_tokens[1, S] int64 -> encoder_out[1, S, 256]
  xlit_decoder.onnx  prev_output_tokens[B, T] int64, encoder_out[1, S, 256]
                     -> lprobs[B, V] (log-softmax of the last position)
  vocab_src.json, vocab_tgt.json

Usage: uv run python export_onnx.py [--opset 17]
"""

from __future__ import annotations

import argparse
import json
from pathlib import Path

import torch

from xlit_ref import CKPT, LANG_TOKENS, TARGET_LANG, load_dicts
from xlit_model import Dictionary, load_checkpoint

ASSETS = Path(__file__).resolve().parents[2] / "assets"


def dump_vocab(d: Dictionary, path: Path) -> None:
    data = {
        "symbols": d.symbols,
        "bos": d.bos,
        "pad": d.pad,
        "eos": d.eos,
        "unk": d.unk,
        "lang_tokens": {tok: d.index[tok] for tok in LANG_TOKENS},
    }
    path.write_text(json.dumps(data, ensure_ascii=False, indent=1), encoding="utf-8")


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--opset", type=int, default=17)
    args = ap.parse_args()

    ASSETS.mkdir(exist_ok=True)
    model, _ = load_checkpoint(CKPT)
    src, tgt = load_dicts()

    src_tokens = torch.tensor([[src.index[f"__{TARGET_LANG}__"], 30, 4, 20, src.eos]])
    with torch.no_grad():
        enc = model.encoder(src_tokens)
    prev = torch.tensor([[tgt.eos, 10, 11], [tgt.eos, 12, 13]])

    torch.onnx.export(
        model.encoder,
        (src_tokens,),
        ASSETS / "xlit_encoder.onnx",
        input_names=["src_tokens"],
        output_names=["encoder_out"],
        dynamic_axes={"src_tokens": {1: "S"}, "encoder_out": {1: "S"}},
        opset_version=args.opset,
        dynamo=False,
    )
    torch.onnx.export(
        model.decoder,
        (prev, enc),
        ASSETS / "xlit_decoder.onnx",
        input_names=["prev_output_tokens", "encoder_out"],
        output_names=["lprobs"],
        dynamic_axes={
            "prev_output_tokens": {0: "B", 1: "T"},
            "encoder_out": {1: "S"},
            "lprobs": {0: "B"},
        },
        opset_version=args.opset,
        dynamo=False,
    )
    dump_vocab(src, ASSETS / "vocab_src.json")
    dump_vocab(tgt, ASSETS / "vocab_tgt.json")
    for f in sorted(ASSETS.iterdir()):
        print(f"{f.name:22s} {f.stat().st_size:>12,d} bytes")


if __name__ == "__main__":
    main()
