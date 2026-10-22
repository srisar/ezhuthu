"""Parity: ONNX (onnxruntime) top-k must equal the PyTorch reference top-k.

Runs both through the same beam search on tests/words.tsv and also reports
the largest log-prob difference seen at any decoder step, top-1 accuracy,
and the ONNX ops used (to check against tract's supported set).

Usage: uv run python parity.py
"""

from __future__ import annotations

import sys
import time
from collections import Counter
from pathlib import Path

import numpy as np
import onnx
import onnxruntime as ort
import torch

from export_onnx import ASSETS
from xlit_ref import BEAM, Reference, beam_search, decode_tokens, encode_source

WORDS = Path(__file__).resolve().parents[2] / "tests" / "words.tsv"


def main() -> int:
    ref = Reference()
    opts = ort.SessionOptions()
    opts.intra_op_num_threads = 1
    enc_sess = ort.InferenceSession(str(ASSETS / "xlit_encoder.onnx"), opts)
    dec_sess = ort.InferenceSession(str(ASSETS / "xlit_decoder.onnx"), opts)

    for name in ("xlit_encoder.onnx", "xlit_decoder.onnx"):
        g = onnx.load(str(ASSETS / name)).graph
        ops = Counter(n.op_type for n in g.node)
        print(f"{name}: {', '.join(sorted(ops))}")

    max_diff = 0.0
    failures = top1 = 0
    onnx_ms = []
    rows = [l.split("\t") for l in WORDS.read_text(encoding="utf-8").splitlines() if l.strip()]
    for word, expected in rows:
        src = encode_source(word, ref.src)
        torch_cands = ref.candidates(word)

        t0 = time.perf_counter()
        enc = enc_sess.run(None, {"src_tokens": np.array([src], dtype=np.int64)})[0]

        def step(tokens: torch.Tensor) -> torch.Tensor:
            out = dec_sess.run(
                None, {"prev_output_tokens": tokens.numpy(), "encoder_out": enc}
            )[0]
            return torch.from_numpy(out)

        hypos = beam_search(step, ref.tgt, len(src), BEAM)
        onnx_ms.append((time.perf_counter() - t0) * 1000)
        onnx_cands = [(decode_tokens(h.tokens, ref.tgt), h.score) for h in hypos]

        # per-step log-prob agreement on the top hypothesis' prefixes
        with torch.no_grad():
            tenc = ref.model.encoder(torch.tensor([src]))
            toks = [ref.tgt.eos] + hypos[0].tokens[:-1]
            for t in range(1, len(toks) + 1):
                prefix = torch.tensor([toks[:t]])
                a = ref.model.decoder(prefix, tenc).numpy()
                b = step(prefix).numpy()
                max_diff = max(max_diff, float(np.abs(a - b).max()))

        same = [w for w, _ in torch_cands] == [w for w, _ in onnx_cands]
        hit = [w for w, _ in onnx_cands]
        rank = hit.index(expected) + 1 if expected in hit else None
        top1 += rank == 1
        failures += (not same) or rank is None
        status = "OK " if same else "DIFF"
        print(f"{status} {word:15s} rank={rank} onnx={hit[0]} ({onnx_ms[-1]:.1f} ms)")
        if not same:
            print(f"     torch: {[w for w, _ in torch_cands]}")
            print(f"     onnx:  {hit}")

    print(f"\nmax |lprob diff| = {max_diff:.2e}")
    print(f"top-1: {top1}/{len(rows)}; onnxruntime 1 thread, beam {BEAM}: "
          f"mean {np.mean(onnx_ms):.1f} ms/word, max {np.max(onnx_ms):.1f} ms")
    return 1 if failures else 0


if __name__ == "__main__":
    sys.exit(main())
