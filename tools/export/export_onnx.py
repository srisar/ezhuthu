"""Export the IndicXlit encoder/decoder to ONNX and dump the vocabularies.

Writes to ../../assets/ (L = 6 layers, H = 4 heads, hd = 64):
  xlit_encoder.onnx  src_tokens[1, S] int64
                     -> cross_k, cross_v [L, 1, H, S, hd]
  xlit_decoder.onnx  one step with a self-attention KV cache:
                     tokens[B, 1] int64, past_k, past_v [L, B, H, P, hd],
                     cross_k, cross_v [L, 1, H, S, hd]
                     -> lprobs[B, V], present_k, present_v [L, B, H, P+1, hd]
  vocab_src.json, vocab_tgt.json

The first step feeds eos with P = 0 (empty past).

Usage: uv run python export_onnx.py [--opset 17]
"""

from __future__ import annotations

import argparse
import json
from pathlib import Path

import torch

from xlit_ref import CKPT, LANG_TOKENS, TARGET_LANG, load_dicts
from xlit_model import CachedDecoderStep, CachedEncoder, Dictionary, load_checkpoint

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
    encoder = CachedEncoder(model.encoder, model.decoder).eval()
    step = CachedDecoderStep(model.decoder).eval()

    src_tokens = torch.tensor([[src.index[f"__{TARGET_LANG}__"], 30, 4, 20, src.eos]])
    with torch.no_grad():
        cross_k, cross_v = encoder(src_tokens)
    layers, _, heads, _, hd = cross_k.shape
    # Trace with B=2, P=3 so neither axis gets specialised to 1.
    tokens = torch.tensor([[10], [11]])
    past = torch.zeros(layers, 2, heads, 3, hd)

    torch.onnx.export(
        encoder,
        (src_tokens,),
        ASSETS / "xlit_encoder.onnx",
        input_names=["src_tokens"],
        output_names=["cross_k", "cross_v"],
        dynamic_axes={"src_tokens": {1: "S"}, "cross_k": {3: "S"}, "cross_v": {3: "S"}},
        opset_version=args.opset,
        dynamo=False,
    )
    torch.onnx.export(
        step,
        (tokens, past, past, cross_k, cross_v),
        ASSETS / "xlit_decoder.onnx",
        input_names=["tokens", "past_k", "past_v", "cross_k", "cross_v"],
        output_names=["lprobs", "present_k", "present_v"],
        dynamic_axes={
            "tokens": {0: "B"},
            "past_k": {1: "B", 3: "P"},
            "past_v": {1: "B", 3: "P"},
            "cross_k": {3: "S"},
            "cross_v": {3: "S"},
            "lprobs": {0: "B"},
            "present_k": {1: "B", 3: "P1"},
            "present_v": {1: "B", 3: "P1"},
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
