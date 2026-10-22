"""Minimal re-implementation of the fairseq transformer used by IndicXlit.

Only what the checkpoint needs: pre-norm layers, GELU, sinusoidal positions,
scaled embeddings, layernorm_embedding, separate output projection. Parameter
names mirror fairseq so the checkpoint's state dict loads with strict=True
(after dropping the version/_float_tensor bookkeeping keys).

Batch-first layout ([B, T, D]) and no padding mask: the app only ever encodes
one word at a time, so source sequences are never padded.
"""

from __future__ import annotations

import argparse
import math
from dataclasses import dataclass
from pathlib import Path

import torch
import torch.nn.functional as F
from torch import Tensor, nn

PAD = 1
# fairseq's SinusoidalPositionalEmbedding init size: max_positions + padding_idx + 1
POS_TABLE_SIZE = 1024 + PAD + 1


@dataclass(frozen=True)
class Config:
    embed_dim: int = 256
    ffn_dim: int = 1024
    heads: int = 4
    layers: int = 6


class Dictionary:
    """fairseq Dictionary: 4 specials, then dict.txt entries, then language tags."""

    SPECIALS = ("<s>", "<pad>", "</s>", "<unk>")

    def __init__(self, symbols: list[str]):
        self.symbols = symbols
        self.index = {s: i for i, s in enumerate(symbols)}

    @classmethod
    def load(cls, dict_txt: Path, lang_tokens: list[str]) -> "Dictionary":
        symbols = list(cls.SPECIALS)
        for line in dict_txt.read_text(encoding="utf-8").splitlines():
            if not line.strip():
                continue
            symbols.append(line.rsplit(" ", 1)[0])
        for tok in lang_tokens:
            if tok not in symbols:
                symbols.append(tok)
        return cls(symbols)

    @property
    def bos(self) -> int:
        return 0

    @property
    def pad(self) -> int:
        return 1

    @property
    def eos(self) -> int:
        return 2

    @property
    def unk(self) -> int:
        return 3

    def __len__(self) -> int:
        return len(self.symbols)


def sinusoidal_table(num_embeddings: int, dim: int, padding_idx: int) -> Tensor:
    """Same formula as fairseq SinusoidalPositionalEmbedding.get_embedding."""
    half = dim // 2
    scale = math.log(10000) / (half - 1)
    freqs = torch.exp(torch.arange(half, dtype=torch.float) * -scale)
    angles = torch.arange(num_embeddings, dtype=torch.float).unsqueeze(1) * freqs.unsqueeze(0)
    table = torch.cat([torch.sin(angles), torch.cos(angles)], dim=1)
    table[padding_idx, :] = 0
    return table


def positions(tokens: Tensor) -> Tensor:
    # fairseq make_positions with no padding: padding_idx + 1 + arange(T)
    t = tokens.shape[1]
    return torch.arange(t, device=tokens.device) + (PAD + 1)


class MultiheadAttention(nn.Module):
    def __init__(self, dim: int, heads: int):
        super().__init__()
        self.heads = heads
        self.head_dim = dim // heads
        self.scaling = self.head_dim**-0.5
        self.q_proj = nn.Linear(dim, dim)
        self.k_proj = nn.Linear(dim, dim)
        self.v_proj = nn.Linear(dim, dim)
        self.out_proj = nn.Linear(dim, dim)

    def _split(self, x: Tensor) -> Tensor:
        b, t, _ = x.shape
        return x.view(b, t, self.heads, self.head_dim).transpose(1, 2)

    def forward(self, query: Tensor, key: Tensor, mask: Tensor | None = None) -> Tensor:
        # key may have batch 1 while query has batch B (cross-attention over
        # one encoded word); MatMul broadcasting handles it.
        q = self._split(self.q_proj(query) * self.scaling)
        k = self._split(self.k_proj(key))
        v = self._split(self.v_proj(key))
        attn = torch.matmul(q, k.transpose(-2, -1))
        if mask is not None:
            attn = attn + mask
        attn = F.softmax(attn, dim=-1)
        out = torch.matmul(attn, v).transpose(1, 2)
        b, t = out.shape[0], out.shape[1]
        return self.out_proj(out.reshape(b, t, -1))


class EncoderLayer(nn.Module):
    def __init__(self, cfg: Config):
        super().__init__()
        self.self_attn = MultiheadAttention(cfg.embed_dim, cfg.heads)
        self.self_attn_layer_norm = nn.LayerNorm(cfg.embed_dim)
        self.fc1 = nn.Linear(cfg.embed_dim, cfg.ffn_dim)
        self.fc2 = nn.Linear(cfg.ffn_dim, cfg.embed_dim)
        self.final_layer_norm = nn.LayerNorm(cfg.embed_dim)

    def forward(self, x: Tensor) -> Tensor:
        h = self.self_attn_layer_norm(x)
        x = x + self.self_attn(h, h)
        h = self.final_layer_norm(x)
        return x + self.fc2(F.gelu(self.fc1(h)))


class DecoderLayer(nn.Module):
    def __init__(self, cfg: Config):
        super().__init__()
        self.self_attn = MultiheadAttention(cfg.embed_dim, cfg.heads)
        self.self_attn_layer_norm = nn.LayerNorm(cfg.embed_dim)
        self.encoder_attn = MultiheadAttention(cfg.embed_dim, cfg.heads)
        self.encoder_attn_layer_norm = nn.LayerNorm(cfg.embed_dim)
        self.fc1 = nn.Linear(cfg.embed_dim, cfg.ffn_dim)
        self.fc2 = nn.Linear(cfg.ffn_dim, cfg.embed_dim)
        self.final_layer_norm = nn.LayerNorm(cfg.embed_dim)

    def forward(self, x: Tensor, enc: Tensor, causal: Tensor) -> Tensor:
        h = self.self_attn_layer_norm(x)
        x = x + self.self_attn(h, h, causal)
        h = self.encoder_attn_layer_norm(x)
        x = x + self.encoder_attn(h, enc)
        h = self.final_layer_norm(x)
        return x + self.fc2(F.gelu(self.fc1(h)))


class Encoder(nn.Module):
    """src_tokens [1, S] -> encoder_out [1, S, D]."""

    def __init__(self, cfg: Config, vocab: int):
        super().__init__()
        self.embed_scale = math.sqrt(cfg.embed_dim)
        self.embed_tokens = nn.Embedding(vocab, cfg.embed_dim, padding_idx=PAD)
        self.register_buffer(
            "pos_table", sinusoidal_table(POS_TABLE_SIZE, cfg.embed_dim, PAD), persistent=False
        )
        self.layernorm_embedding = nn.LayerNorm(cfg.embed_dim)
        self.layers = nn.ModuleList(EncoderLayer(cfg) for _ in range(cfg.layers))
        self.layer_norm = nn.LayerNorm(cfg.embed_dim)

    def forward(self, src_tokens: Tensor) -> Tensor:
        x = self.embed_scale * self.embed_tokens(src_tokens) + self.pos_table[positions(src_tokens)]
        x = self.layernorm_embedding(x)
        for layer in self.layers:
            x = layer(x)
        return self.layer_norm(x)


class Decoder(nn.Module):
    """(prev_output_tokens [B, T], encoder_out [1, S, D]) -> log-probs [B, V] of the last step.

    No KV cache: the whole prefix is re-run each step, which is cheap for
    single words. Only the last position is projected to the vocabulary.
    """

    def __init__(self, cfg: Config, vocab: int):
        super().__init__()
        self.embed_scale = math.sqrt(cfg.embed_dim)
        self.embed_tokens = nn.Embedding(vocab, cfg.embed_dim, padding_idx=PAD)
        self.register_buffer(
            "pos_table", sinusoidal_table(POS_TABLE_SIZE, cfg.embed_dim, PAD), persistent=False
        )
        self.layernorm_embedding = nn.LayerNorm(cfg.embed_dim)
        self.layers = nn.ModuleList(DecoderLayer(cfg) for _ in range(cfg.layers))
        self.layer_norm = nn.LayerNorm(cfg.embed_dim)
        self.output_projection = nn.Linear(cfg.embed_dim, vocab, bias=False)

    def forward(self, prev_output_tokens: Tensor, encoder_out: Tensor) -> Tensor:
        t = prev_output_tokens.shape[1]
        x = self.embed_scale * self.embed_tokens(prev_output_tokens)
        x = self.layernorm_embedding(x + self.pos_table[positions(prev_output_tokens)])
        # Causal mask without Trilu (keeps the graph simple for tract).
        idx = torch.arange(t, device=x.device)
        causal = torch.where(
            idx.unsqueeze(0) > idx.unsqueeze(1),
            torch.tensor(float("-inf")),
            torch.tensor(0.0),
        )
        for layer in self.layers:
            x = layer(x, encoder_out, causal)
        x = self.layer_norm(x[:, -1, :])
        return F.log_softmax(self.output_projection(x), dim=-1)


class Xlit(nn.Module):
    def __init__(self, cfg: Config, src_vocab: int, tgt_vocab: int):
        super().__init__()
        self.encoder = Encoder(cfg, src_vocab)
        self.decoder = Decoder(cfg, tgt_vocab)


def load_checkpoint(path: Path) -> tuple[Xlit, dict]:
    """Load the fairseq checkpoint into Xlit. Returns (model, model cfg dict)."""
    torch.serialization.add_safe_globals([argparse.Namespace])
    ck = torch.load(path, map_location="cpu", weights_only=True)
    mcfg = ck["cfg"]["model"]
    if isinstance(mcfg, argparse.Namespace):
        mcfg = vars(mcfg)
    expected = dict(
        arch="transformer",
        activation_fn="gelu",
        encoder_normalize_before=True,
        decoder_normalize_before=True,
        layernorm_embedding=True,
        encoder_learned_pos=False,
        decoder_learned_pos=False,
        no_scale_embedding=False,
        share_all_embeddings=False,
        share_decoder_input_output_embed=False,
        no_token_positional_embeddings=False,
    )
    for k, v in expected.items():
        if mcfg[k] != v:
            raise ValueError(f"checkpoint has {k}={mcfg[k]!r}, this port assumes {v!r}")
    cfg = Config(
        embed_dim=mcfg["encoder_embed_dim"],
        ffn_dim=mcfg["encoder_ffn_embed_dim"],
        heads=mcfg["encoder_attention_heads"],
        layers=mcfg["encoder_layers"],
    )
    state = {
        k: v
        for k, v in ck["model"].items()
        if not k.endswith(".version") and not k.endswith("embed_positions._float_tensor")
    }
    model = Xlit(
        cfg,
        src_vocab=state["encoder.embed_tokens.weight"].shape[0],
        tgt_vocab=state["decoder.embed_tokens.weight"].shape[0],
    )
    model.load_state_dict(state, strict=True)
    model.eval()
    return model, mcfg
