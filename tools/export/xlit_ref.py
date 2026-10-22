"""Reference transliteration in Python: preprocessing, beam search, rescoring.

Mirrors the official ai4bharat-transliteration engine
(app/ai4bharat/transliteration/transformer/base_engine.py) and fairseq's
SequenceGenerator/BeamSearch for a single sentence. The Rust port in
crates/xlit must produce the same top-k as this file.

Usage: uv run python xlit_ref.py natchathiram [more words...]
"""

from __future__ import annotations

import math
import re
import sys
from dataclasses import dataclass
from pathlib import Path
from typing import Callable

import torch

from xlit_model import Dictionary, Xlit, load_checkpoint

HERE = Path(__file__).resolve().parent
WORK = HERE / "work"
CKPT = WORK / "model" / "transformer" / "indicxlit.pt"
SRC_DICT = WORK / "model" / "corpus-bin" / "dict.en.txt"
TGT_DICT = WORK / "model" / "corpus-bin" / "dict.ta.txt"

# app/ai4bharat/transliteration/transformer/models/en2indic/lang_list.txt
# (not shipped in the checkpoint zip). Order decides the language-tag ids.
LANG_LIST = (
    "en as bn brx gom gu hi kn ks mai ml mni mr ne or pa sa sd si ta te ur".split()
)
LANG_TOKENS = [f"__{lang}__" for lang in LANG_LIST]
TARGET_LANG = "ta"

# Official defaults: beam_width=4, rescore alpha=0.9. We decode with a wider
# beam because the app shows five candidates.
BEAM = 5
MAX_LEN_B = 200  # fairseq default max_len_b (max_len_a = 0)
RESCORE_ALPHA = 0.9

LATIN_RUN = re.compile(r"[a-z]+")


def load_dicts() -> tuple[Dictionary, Dictionary]:
    return (
        Dictionary.load(SRC_DICT, LANG_TOKENS),
        Dictionary.load(TGT_DICT, LANG_TOKENS),
    )


def encode_source(word: str, src: Dictionary, lang: str = TARGET_LANG) -> list[int]:
    """`__ta__ n a t ... </s>`; the engine lowercases and keeps a-z only."""
    word = word.lower().strip()
    runs = LATIN_RUN.findall(word)
    if not runs:
        raise ValueError(f"no latin letters in {word!r}")
    chars = runs[-1]
    return [src.index[f"__{lang}__"]] + [src.index.get(c, src.unk) for c in chars] + [src.eos]


@dataclass
class Hypo:
    tokens: list[int]  # generated tokens, ending with eos
    score: float  # sum of log-probs / len (natural log), fairseq "score"


def beam_search(
    step_fn: Callable[[torch.Tensor], torch.Tensor],
    tgt: Dictionary,
    src_len: int,
    beam: int = BEAM,
) -> list[Hypo]:
    """fairseq SequenceGenerator._generate + BeamSearch.step, bsz=1.

    step_fn(tokens [n, t]) -> log-probs [n, V] for the next token.
    Defaults: lenpen=1, normalize_scores=True, unk_penalty=0, min_len=1.
    """
    eos, pad, unk = tgt.eos, tgt.pad, tgt.unk
    vocab = len(tgt)
    max_len = min(MAX_LEN_B, 1024 - 1)
    _ = src_len  # max_len_a = 0, so the source length does not matter

    tokens = torch.full((beam, max_len + 2), pad, dtype=torch.long)
    tokens[:, 0] = eos
    scores = torch.zeros(beam, max_len + 1)
    finalized: list[Hypo] = []
    cand_size = 2 * beam

    for step in range(max_len + 1):
        lprobs = step_fn(tokens[:, : step + 1]).clone()
        lprobs[lprobs != lprobs] = -math.inf
        lprobs[:, pad] = -math.inf
        lprobs[:, unk] -= 0.0
        if step >= max_len:
            lprobs[:, :eos] = -math.inf
            lprobs[:, eos + 1 :] = -math.inf
        if step < 1:  # min_len
            lprobs[:, eos] = -math.inf

        # BeamSearch.step
        if step == 0:
            lp = lprobs[:1]
        else:
            lp = lprobs + scores[:, step - 1].unsqueeze(-1)
        flat = lp.reshape(-1)
        cand_scores, idx = torch.topk(flat, k=min(cand_size, flat.numel() - 1))
        cand_tokens = idx % vocab
        cand_beams = torch.div(idx, vocab, rounding_mode="trunc")

        # finalize hypotheses ending in eos among the top `beam` candidates
        eos_mask = (cand_tokens == eos) & (cand_scores != -math.inf)
        for j in range(beam):
            if not eos_mask[j]:
                continue
            b = int(cand_beams[j])
            seq = tokens[b, 1 : step + 1].tolist() + [eos]
            finalized.append(Hypo(seq, float(cand_scores[j]) / (step + 1)))
            if len(finalized) == beam:
                break
        if len(finalized) >= beam:
            break

        # keep the first `beam` non-eos candidates, in candidate order
        active = [j for j in range(cand_scores.numel()) if not eos_mask[j]][:beam]
        sel_beams = cand_beams[active]
        tokens[:, : step + 1] = tokens[sel_beams, : step + 1]
        tokens[:, step + 1] = cand_tokens[active]
        scores[:, :step] = scores[sel_beams, :step]
        scores[:, step] = cand_scores[active]

    finalized.sort(key=lambda h: h.score, reverse=True)
    return finalized[:beam]


def decode_tokens(tokens: list[int], tgt: Dictionary) -> str:
    return "".join(tgt.symbols[t] for t in tokens if t != tgt.eos)


def rescore(
    candidates: list[tuple[str, float]],
    word_prob: dict[str, float] | None,
    alpha: float = RESCORE_ALPHA,
) -> list[tuple[str, float]]:
    """base_engine.rescore: alpha * beam share + (1 - alpha) * dict share.

    candidates: (word, fairseq score in natural log). The engine works with
    2 ** (score / ln 2) == exp(score). Words missing from the dict get 0
    total, so they sink below every in-dict word; ties keep beam order.
    """
    if not word_prob:
        return candidates
    beam_p = [math.exp(s) for _, s in candidates]
    beam_total = sum(beam_p)
    in_dict = [word_prob[w] for w, _ in candidates if w in word_prob]
    dict_total = sum(in_dict)
    out = []
    for (w, _), bp in zip(candidates, beam_p):
        if w in word_prob:
            out.append((w, alpha * bp / beam_total + (1 - alpha) * word_prob[w] / dict_total))
        else:
            out.append((w, 0.0))
    return sorted(out, key=lambda x: x[1], reverse=True)


class Reference:
    def __init__(self, ckpt: Path = CKPT):
        self.model, _ = load_checkpoint(ckpt)
        self.src, self.tgt = load_dicts()
        assert len(self.src) == self.model.encoder.embed_tokens.num_embeddings
        assert len(self.tgt) == self.model.decoder.embed_tokens.num_embeddings

    @torch.no_grad()
    def candidates(self, word: str, beam: int = BEAM) -> list[tuple[str, float]]:
        src_tokens = torch.tensor([encode_source(word, self.src)])
        enc = self.model.encoder(src_tokens)
        hypos = beam_search(
            lambda toks: self.model.decoder(toks, enc), self.tgt, src_tokens.shape[1], beam
        )
        return [(decode_tokens(h.tokens, self.tgt), h.score) for h in hypos]


def write_golden(words_file: Path, rescored: bool) -> None:
    """word<TAB>cand1|cand2|... for the Rust parity tests.

    Raw beam order -> tests/golden_top5.tsv; with --rescored (using
    assets/ta_word_freq.bin) -> tests/golden_rescored_top5.tsv.
    """
    ref = Reference()
    word_prob = None
    if rescored:
        from word_freq import load_bin

        word_prob = load_bin()
    out = []
    for word in words_file.read_text(encoding="utf-8").splitlines():
        if word.strip():
            cands = ref.candidates(word)
            if word_prob is not None:
                cands = rescore(cands, word_prob)
            out.append(word + "\t" + "|".join(w for w, _ in cands))
    sys.stdout.buffer.write(("\n".join(out) + "\n").encode("utf-8"))


def main(words: list[str]) -> None:
    if words[:1] in (["--golden"], ["--golden-rescored"]):
        write_golden(Path(words[1]), rescored=words[0] == "--golden-rescored")
        return
    ref = Reference()
    for word in words:
        cands = ref.candidates(word)
        print(word)
        for w, s in cands:
            print(f"  {w}\t{s:.4f}")


if __name__ == "__main__":
    main(sys.argv[1:] or ["natchathiram"])
