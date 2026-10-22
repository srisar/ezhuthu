"""Build assets/ta_word_freq.bin from IndicXlit's Tamil word-probability dict.

The official dicts ship as one 850 MB zip; the Tamil JSON alone is 1.04 GB
uncompressed. `fetch` downloads only the Tamil entry's compressed bytes
(about 105 MB) using HTTP range requests against the zip's central
directory. `build` stream-inflates it and keeps the N most probable words.

The JSON is pretty-printed one `"word": prob,` per line, sorted by
probability descending (checked while reading), so the top N are the first N.

ta_word_freq.bin (little-endian):
  magic  b"EZWF"   version u32 = 1   count u32
  count x (offset u32)       byte offset of each record, sorted by word bytes
  records: len u8, word bytes, prob f32

Word bytes use a one-byte-per-character code instead of UTF-8 (which spends
3 bytes on every Tamil letter): U+0B80..U+0BFF (the Tamil block) -> 0x80 +
(cp - 0x0B80), ASCII stays as is. Words with any other character are
dropped. Records are sorted by these bytes, so lookups encode the query the
same way and binary-search.

Usage:
  uv run python word_freq.py fetch
  uv run python word_freq.py build [--top 500000]
  uv run python word_freq.py stats      # coverage of test/parity candidates
"""

from __future__ import annotations

import argparse
import json
import struct
import sys
import urllib.request
import zlib
from pathlib import Path

from export_onnx import ASSETS

ZIP_URL = "https://github.com/AI4Bharat/IndicXlit/releases/download/v1.0/word_prob_dicts.zip"
ENTRY = "word_prob_dicts/ta_word_prob_dict.json"
WORK = Path(__file__).resolve().parent / "work" / "word_prob"
RAW = WORK / "ta_word_prob_dict.json.deflate"
OUT = ASSETS / "ta_word_freq.bin"
MAGIC = b"EZWF"


def _range(url: str, start: int, end: int) -> bytes:
    req = urllib.request.Request(url, headers={"Range": f"bytes={start}-{end}"})
    with urllib.request.urlopen(req) as r:
        if r.status != 206:
            raise RuntimeError(f"server ignored the Range header (HTTP {r.status})")
        return r.read()


def _resolve(url: str) -> tuple[str, int]:
    """Follow redirects once; return the final URL and total size."""
    req = urllib.request.Request(url, method="HEAD")
    with urllib.request.urlopen(req) as r:
        return r.url, int(r.headers["Content-Length"])


def fetch() -> None:
    WORK.mkdir(parents=True, exist_ok=True)
    url, size = _resolve(ZIP_URL)
    tail = _range(url, size - 65536, size - 1)
    eocd = tail.rfind(b"PK\x05\x06")
    if eocd < 0:
        raise RuntimeError("end of central directory not found")
    cd_size, cd_offset = struct.unpack_from("<II", tail, eocd + 12)
    cd = _range(url, cd_offset, cd_offset + cd_size - 1)

    pos = 0
    while pos < len(cd):
        if cd[pos : pos + 4] != b"PK\x01\x02":
            raise RuntimeError("bad central directory entry")
        method, = struct.unpack_from("<H", cd, pos + 10)
        csize, usize, nlen, xlen, clen = struct.unpack_from("<IIHHH", cd, pos + 20)
        local, = struct.unpack_from("<I", cd, pos + 42)
        name = cd[pos + 46 : pos + 46 + nlen].decode()
        pos += 46 + nlen + xlen + clen
        if name == ENTRY:
            break
    else:
        raise RuntimeError(f"{ENTRY} not in zip")
    if method != 8:
        raise RuntimeError(f"unexpected compression method {method}")

    lh = _range(url, local, local + 29)
    lnlen, lxlen = struct.unpack_from("<HH", lh, 26)
    start = local + 30 + lnlen + lxlen
    print(f"{ENTRY}: {csize:,} bytes compressed, {usize:,} uncompressed")

    req = urllib.request.Request(url, headers={"Range": f"bytes={start}-{start + csize - 1}"})
    done = 0
    with urllib.request.urlopen(req) as r, open(RAW, "wb") as f:
        while chunk := r.read(1 << 20):
            f.write(chunk)
            done += len(chunk)
            print(f"\r  {done / csize:6.1%}", end="", flush=True)
    print()
    if done != csize:
        raise RuntimeError(f"short download: {done} of {csize} bytes")


def iter_entries():
    """Yield (word, prob) in file order by stream-inflating the raw entry."""
    inflate = zlib.decompressobj(-15)
    buf = b""
    with open(RAW, "rb") as f:
        while chunk := f.read(1 << 20):
            buf += inflate.decompress(chunk)
            *lines, buf = buf.split(b"\n")
            for line in lines:
                line = line.strip().rstrip(b",")
                if line in (b"{", b"}", b""):
                    continue
                key, _, val = line.rpartition(b":")
                yield json.loads(key), float(val)
    buf += inflate.flush()
    for line in buf.split(b"\n"):
        line = line.strip().rstrip(b",")
        if line not in (b"{", b"}", b""):
            key, _, val = line.rpartition(b":")
            yield json.loads(key), float(val)


def encode_word(word: str) -> bytes | None:
    out = bytearray()
    for ch in word:
        cp = ord(ch)
        if cp < 0x80:
            out.append(cp)
        elif 0x0B80 <= cp <= 0x0BFF:
            out.append(0x80 + cp - 0x0B80)
        else:
            return None
    return bytes(out)


def decode_word(b: bytes) -> str:
    return "".join(chr(x) if x < 0x80 else chr(0x0B80 + x - 0x80) for x in b)


def build(top: int) -> None:
    words: list[tuple[bytes, float]] = []
    prev = float("inf")
    dropped = 0
    for word, prob in iter_entries():
        if prob > prev:
            raise RuntimeError(f"not sorted by probability at {word!r}")
        prev = prob
        b = encode_word(word)
        if b is None or not 0 < len(b) <= 255:
            dropped += 1
            continue
        words.append((b, prob))
        if len(words) == top:
            break
    words.sort(key=lambda w: w[0])
    offsets, records, off = [], bytearray(), 0
    for b, prob in words:
        offsets.append(off)
        rec = struct.pack("<B", len(b)) + b + struct.pack("<f", prob)
        records += rec
        off += len(rec)
    header = MAGIC + struct.pack("<II", 1, len(words))
    OUT.write_bytes(header + struct.pack(f"<{len(offsets)}I", *offsets) + records)
    print(f"{OUT.name}: {len(words):,} words, {OUT.stat().st_size:,} bytes, "
          f"lowest kept prob {prev:.3g}, {dropped:,} non-Tamil words skipped")


def load_bin(path: Path = OUT) -> dict[str, float]:
    data = path.read_bytes()
    if data[:4] != MAGIC:
        raise ValueError("not an ezhuthu word-freq file")
    _version, count = struct.unpack_from("<II", data, 4)
    base = 12 + 4 * count
    out = {}
    for (off,) in struct.iter_unpack("<I", data[12:base]):
        n = data[base + off]
        word = decode_word(data[base + off + 1 : base + off + 1 + n])
        out[word], = struct.unpack_from("<f", data, base + off + 1 + n)
    return out


def stats() -> None:
    """How many beam candidates of the parity words each cutoff would cover."""
    root = Path(__file__).resolve().parents[2]
    cands = set()
    for line in (root / "tests" / "golden_top5.tsv").read_text(encoding="utf-8").splitlines():
        if line:
            cands.update(line.split("\t")[1].split("|"))
    expected = {l.split("\t")[1] for l in (root / "tests" / "words.tsv").read_text(encoding="utf-8").splitlines() if l}
    rank: dict[str, int] = {}
    total = 0
    for i, (word, _) in enumerate(iter_entries()):
        total = i + 1
        if word in cands and word not in rank:
            rank[word] = total
    print(f"total entries: {total:,}")
    for cut in (100_000, 250_000, 500_000, 1_000_000, 2_000_000, total):
        c = sum(1 for r in rank.values() if r <= cut)
        e = sum(1 for w in expected if rank.get(w, total + 1) <= cut)
        print(f"top {cut:>10,}: {c:3d}/{len(cands)} candidates, {e}/{len(expected)} expected words")
    missing = sorted(w for w in expected if w not in rank)
    print("expected words absent from the dict:", missing or "none")


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("cmd", choices=["fetch", "build", "stats"])
    ap.add_argument("--top", type=int, default=500_000)
    args = ap.parse_args()
    {"fetch": fetch, "build": lambda: build(args.top), "stats": stats}[args.cmd]()
    return 0


if __name__ == "__main__":
    sys.exit(main())
