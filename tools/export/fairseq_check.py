"""Independent check: run the checkpoint through real fairseq 0.12.2.

Not part of the uv project. fairseq's C++ extensions don't build here, but
generation only needs the pure-Python package, so it runs from the unpacked
sdist on PYTHONPATH inside a throwaway venv (see README.md, "Verifying against
fairseq"). Prints the same format as xlit_ref.py so the two can be diffed.

Usage: python fairseq_check.py <model_dir> <beam> word [word...]
"""

import sys

import torch
from fairseq import checkpoint_utils
from fairseq.sequence_generator import SequenceGenerator


def main() -> None:
    model_dir, beam, words = sys.argv[1], int(sys.argv[2]), sys.argv[3:]
    langs = [l.strip() for l in open(f"{model_dir}/lang_list.txt") if l.strip()]
    overrides = {
        "data": f"{model_dir}/corpus-bin",
        "lang_dict": f"{model_dir}/lang_list.txt",
        "lang_pairs": ",".join(f"en-{l}" for l in langs if l != "en"),
        "source_lang": "en",
        "target_lang": "ta",
    }
    models, _cfg, task = checkpoint_utils.load_model_ensemble_and_task(
        [f"{model_dir}/transformer/indicxlit.pt"], arg_overrides=overrides
    )
    for m in models:
        m.eval()
    src_dict, tgt_dict = task.source_dictionary, task.target_dictionary
    gen = SequenceGenerator(models, tgt_dict, beam_size=beam, max_len_b=200)
    for word in words:
        ids = [src_dict.index("__ta__")] + [src_dict.index(c) for c in word.lower()]
        ids.append(src_dict.eos())
        src = torch.tensor([ids])
        sample = {"net_input": {"src_tokens": src, "src_lengths": torch.tensor([len(ids)])}}
        with torch.no_grad():
            hypos = gen.generate(models, sample)[0]
        print(word)
        for h in hypos[:beam]:
            text = tgt_dict.string(h["tokens"]).replace(" ", "")
            print(f"  {text}\t{h['score'].item():.4f}")


if __name__ == "__main__":
    main()
