#!/usr/bin/env python3
"""Transform `account-driver-reader --dump-full-proof` output into the two JSON
files that `cartesi-rollups-cli prove-drive-root` and `withdraw` consume.

The reader emits ONE full Merkle proof (account leaf -> machine state root) with
base64 hashes. The CLI instead wants the proof split in two:

  * prove-drive-root  ->  { accounts_drive_merkle_root, proof }
        proof = the siblings ABOVE the accounts-drive root (drive root -> machine root)
  * withdraw          ->  { account, account_index, account_root_siblings }
        account_root_siblings = the siblings BELOW it (account leaf -> drive root)

The split point is level `log2(accounts_region) - log2_target_size`. For this
wallet: accounts region = 2^(5 + log2_max_num_of_accounts + log2_leaves_per_account)
= 2^(5+12+2) = 2^19 bytes, and log2_target_size = 7, so we split after 12 siblings.

Folding direction at each level is taken from the bits of the target address, and
the whole chain is folded and checked against the reader's `root_hash` — so a wrong
split, order, or hash function fails loudly here instead of on-chain.

Usage:
  transform_proof.py FULL_PROOF.json ACCOUNTS_DRIVE.bin OUT_DIR \
      [--split N] [--target-addr 0x90000000000000]
"""
import argparse
import base64
import json
import os
import sys


# ----------------------------------------------------------------------------
# Keccak-256 (Ethereum variant; NOT NIST SHA3 — different padding byte).
# ----------------------------------------------------------------------------
def keccak256(msg: bytes) -> bytes:
    RC = [
        0x0000000000000001, 0x0000000000008082, 0x800000000000808A, 0x8000000080008000,
        0x000000000000808B, 0x0000000080000001, 0x8000000080008081, 0x8000000000008009,
        0x000000000000008A, 0x0000000000000088, 0x0000000080008009, 0x000000008000000A,
        0x000000008000808B, 0x800000000000008B, 0x8000000000008089, 0x8000000000008003,
        0x8000000000008002, 0x8000000000000080, 0x000000000000800A, 0x800000008000000A,
        0x8000000080008081, 0x8000000000008080, 0x0000000080000001, 0x8000000080008008,
    ]
    ROT = [
        [0, 36, 3, 41, 18],
        [1, 44, 10, 45, 2],
        [62, 6, 43, 15, 61],
        [28, 55, 25, 21, 56],
        [27, 20, 39, 8, 14],
    ]
    M = 0xFFFFFFFFFFFFFFFF

    def rol(x, n):
        return ((x << n) | (x >> (64 - n))) & M

    st = [[0] * 5 for _ in range(5)]
    rate = 136  # bytes, for keccak-256

    data = bytearray(msg)
    data.append(0x01)               # keccak padding
    while len(data) % rate != 0:
        data.append(0)
    data[-1] ^= 0x80

    for off in range(0, len(data), rate):
        block = data[off:off + rate]
        for i in range(rate // 8):
            lane = int.from_bytes(block[i * 8:i * 8 + 8], 'little')
            st[i % 5][i // 5] ^= lane
        for rnd in range(24):
            C = [st[x][0] ^ st[x][1] ^ st[x][2] ^ st[x][3] ^ st[x][4] for x in range(5)]
            D = [C[(x - 1) % 5] ^ rol(C[(x + 1) % 5], 1) for x in range(5)]
            for x in range(5):
                for y in range(5):
                    st[x][y] ^= D[x]
            B = [[0] * 5 for _ in range(5)]
            for x in range(5):
                for y in range(5):
                    B[y][(2 * x + 3 * y) % 5] = rol(st[x][y], ROT[x][y])
            for x in range(5):
                for y in range(5):
                    st[x][y] = B[x][y] ^ ((~B[(x + 1) % 5][y]) & B[(x + 2) % 5][y])
            st[0][0] ^= RC[rnd]

    out = bytearray()
    for i in range(4):  # 4 lanes = 32 bytes
        out += st[i % 5][i // 5].to_bytes(8, 'little')
    return bytes(out[:32])


# Self-test: empty-string keccak-256.
assert keccak256(b"").hex() == \
    "c5d2460186f7233c927e7db2dcc703c0e500b653ca82273b7bfad8045d85a470", \
    "keccak256 implementation is broken"


def hx(b: bytes) -> str:
    return "0x" + b.hex()


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("full_proof")
    ap.add_argument("drive_bin")
    ap.add_argument("out_dir")
    ap.add_argument("--split", type=int, default=12,
                    help="number of siblings belonging to the account subtree (default 12)")
    ap.add_argument("--target-addr", default="0x90000000000000",
                    help="accounts-drive start address (default 0x90000000000000)")
    ap.add_argument("--account-index", default="0x0")
    ap.add_argument("--account-bytes", type=int, default=128)
    args = ap.parse_args()

    p = json.load(open(args.full_proof))
    log2_target = p["log2_target_size"]
    target_addr = int(args.target_addr, 16)
    target_hash = base64.b64decode(p["target_hash"])
    root_hash = base64.b64decode(p["root_hash"])
    sibs = [base64.b64decode(s) for s in p["sibling_hashes"]]

    if len(sibs) != 64 - log2_target:
        print(f"WARNING: expected {64 - log2_target} siblings, got {len(sibs)}", file=sys.stderr)

    # Fold the whole chain; direction per target-address bit at each level.
    h = target_hash
    drive_root = None
    for i, sib in enumerate(sibs):
        bit = (target_addr >> (log2_target + i)) & 1
        h = keccak256((sib + h) if bit else (h + sib))
        if i + 1 == args.split:
            drive_root = h  # accounts-drive Merkle root (node at the split level)

    if drive_root is None:
        sys.exit(f"split {args.split} >= sibling count {len(sibs)}")
    if h != root_hash:
        sys.exit("FOLD MISMATCH: recomputed root != reader root_hash — "
                 "wrong split/direction/hash. Refusing to emit bad proofs.")
    print("[ok] full fold reproduces reader root_hash")

    account = open(args.drive_bin, "rb").read(args.account_bytes)
    account_root_siblings = [hx(s) for s in sibs[:args.split]]
    drive_proof = [hx(s) for s in sibs[args.split:]]

    os.makedirs(args.out_dir, exist_ok=True)
    drp = os.path.join(args.out_dir, "drive-root-proof.json")
    wp = os.path.join(args.out_dir, "withdraw-proof.json")
    json.dump({"accounts_drive_merkle_root": hx(drive_root), "proof": drive_proof},
              open(drp, "w"), indent=2)
    json.dump({"account": hx(account),
               "account_index": args.account_index,
               "account_root_siblings": account_root_siblings},
              open(wp, "w"), indent=2)

    print(f"[ok] accounts_drive_merkle_root = {hx(drive_root)}")
    print(f"[ok] drive-root proof siblings  = {len(drive_proof)}")
    print(f"[ok] account_root_siblings      = {len(account_root_siblings)}")
    print(f"[ok] account ({len(account)} bytes)        = {hx(account)}")
    print(f"[ok] wrote {drp}")
    print(f"[ok] wrote {wp}")


if __name__ == "__main__":
    main()
