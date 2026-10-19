#!/usr/bin/env python3
"""Independent genesis verifier for vTorrent-NG.

This script does not use the Rust decoder. It parses the embedded legacy
snapshot, rebuilds the genesis transactions, and recomputes the transaction
ids, merkle root, UTXO commitment, and block hash from the documented
serialization rules.

Run from the repository root:

    python3 scripts/verify-genesis.py
"""

import hashlib
import struct
import sys
from pathlib import Path

SNAPSHOT = Path(__file__).resolve().parent.parent / "vtorrent-node/src/genesis_snapshot.bin"

GENESIS_MESSAGE = (
    "vTorrent 2.0 - Revived 2025 - Old holders made whole - No exchange needed"
).encode()
GENESIS_TIMESTAMP = 1_700_000_000
GENESIS_BITS = 0x1E0FFFFF
LEGACY_SNAPSHOT_HEIGHT = 1_680_456
LEGACY_ADDRESS_COUNT = 59_375
LEGACY_TOTAL_SUPPLY_SATOSHIS = 1_158_974_663_136_877
EXPECTED_GENESIS_HASH = bytes.fromhex(
    "36ca792aa45ca7850f2789ff2e62ec13e91bd5f2770d6fea8df81bc2da1da8f8"
)
EXPECTED_UTXO_ROOT = bytes.fromhex(
    "65185f8a5c055c17bf7053c6b6c42993565bb5586689a8508017005b842f9105"
)

TX_TYPE_COINBASE = 1
TX_TYPE_LEGACY_CLAIM = 3


def sha256d(data: bytes) -> bytes:
    return hashlib.sha256(hashlib.sha256(data).digest()).digest()


def compact_size(value: int) -> bytes:
    if value < 0xFD:
        return bytes([value])
    if value <= 0xFFFF:
        return b"\xfd" + struct.pack("<H", value)
    if value <= 0xFFFF_FFFF:
        return b"\xfe" + struct.pack("<I", value)
    return b"\xff" + struct.pack("<Q", value)


def bincode_vec(items) -> bytes:
    return struct.pack("<Q", len(items)) + b"".join(items)


def bincode_bytes(data: bytes) -> bytes:
    return struct.pack("<Q", len(data)) + data


def bincode_option(value) -> bytes:
    return b"\x00" if value is None else b"\x01" + value


def bincode_tx(version, tx_type, inputs, outputs, lock_time, claim_address=None,
               claim_signature=None) -> bytes:
    encoded_inputs = [
        prev_txid
        + struct.pack("<I", prev_vout)
        + bincode_bytes(script_sig)
        + struct.pack("<I", sequence)
        for prev_txid, prev_vout, script_sig, sequence in inputs
    ]
    encoded_outputs = [
        struct.pack("<Q", value) + bincode_bytes(script_pubkey)
        for value, script_pubkey in outputs
    ]
    return (
        struct.pack("<I", version)
        + struct.pack("<I", tx_type)
        + bincode_vec(encoded_inputs)
        + bincode_vec(encoded_outputs)
        + struct.pack("<I", lock_time)
        + bincode_option(claim_address)
        + bincode_option(claim_signature)
    )


def txid(raw: bytes) -> bytes:
    return sha256d(raw)


def merkle_root(leaves) -> bytes:
    if not leaves:
        return b"\x00" * 32
    level = list(leaves)
    while len(level) > 1:
        if len(level) % 2:
            level.append(level[-1])
        level = [sha256d(level[i] + level[i + 1]) for i in range(0, len(level), 2)]
    return level[0]


def hash_utxo(txid_bytes, vout, value, script, height, timestamp) -> bytes:
    preimage = (
        txid_bytes
        + struct.pack("<I", vout)
        + struct.pack("<Q", value)
        + compact_size(len(script))
        + script
        + struct.pack("<I", height)
        + struct.pack("<I", timestamp)
    )
    return sha256d(preimage)


def bincode_header(version, prev_hash, merkle, utxo_root, timestamp, bits, nonce,
                   stake_modifier) -> bytes:
    return (
        struct.pack("<I", version)
        + prev_hash
        + merkle
        + utxo_root
        + struct.pack("<I", timestamp)
        + struct.pack("<I", bits)
        + struct.pack("<I", nonce)
        + struct.pack("<Q", stake_modifier)
    )


BASE58_ALPHABET = "123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz"


def base58check_decode(address: str) -> bytes:
    value = 0
    for char in address:
        value = value * 58 + BASE58_ALPHABET.index(char)
    raw = value.to_bytes((value.bit_length() + 7) // 8, "big")
    leading = len(address) - len(address.lstrip("1"))
    raw = b"\x00" * leading + raw
    if len(raw) != 25:
        raise ValueError(f"address {address!r} has {len(raw)} bytes, expected 25")
    payload, checksum = raw[:-4], raw[-4:]
    if sha256d(payload)[:4] != checksum:
        raise ValueError(f"address {address!r} has an invalid checksum")
    if payload[0] != 70:
        raise ValueError(f"address {address!r} has version {payload[0]}, expected 70")
    return payload


def parse_snapshot(path: Path):
    data = path.read_bytes()
    if len(data) < 4:
        raise ValueError("snapshot is shorter than its count prefix")
    count = struct.unpack("<I", data[:4])[0]
    if count != LEGACY_ADDRESS_COUNT:
        raise ValueError(f"snapshot count {count} != {LEGACY_ADDRESS_COUNT}")
    if len(data) != 4 + count * 42:
        raise ValueError(f"snapshot length {len(data)} != {4 + count * 42}")
    entries = []
    seen = set()
    total = 0
    for index in range(count):
        offset = 4 + index * 42
        raw_address = data[offset:offset + 34]
        address = raw_address.split(b"\x00", 1)[0].decode("ascii")
        balance = struct.unpack("<Q", data[offset + 34:offset + 42])[0]
        base58check_decode(address)
        if address in seen:
            raise ValueError(f"duplicate snapshot address {address!r}")
        seen.add(address)
        total += balance
        entries.append((address, balance))
    if total != LEGACY_TOTAL_SUPPLY_SATOSHIS:
        raise ValueError(f"snapshot total {total} != {LEGACY_TOTAL_SUPPLY_SATOSHIS}")
    return entries


def main() -> int:
    entries = parse_snapshot(SNAPSHOT)
    print(f"snapshot: {len(entries)} unique addresses, {LEGACY_TOTAL_SUPPLY_SATOSHIS} sats")

    coinbase_raw = bincode_tx(
        version=1,
        tx_type=TX_TYPE_COINBASE,
        inputs=[],
        outputs=[(0, GENESIS_MESSAGE)],
        lock_time=0,
    )
    coinbase_txid = txid(coinbase_raw)

    legacy_outputs = [
        (balance, b"\x6a" + bytes([len(address)]) + address.encode("ascii"))
        for address, balance in entries
    ]
    legacy_raw = bincode_tx(
        version=1,
        tx_type=TX_TYPE_LEGACY_CLAIM,
        inputs=[],
        outputs=legacy_outputs,
        lock_time=LEGACY_SNAPSHOT_HEIGHT,
    )
    legacy_txid = txid(legacy_raw)

    merkle = merkle_root([coinbase_txid, legacy_txid])

    utxos = [(coinbase_txid, 0, 0, GENESIS_MESSAGE)]
    utxos.extend(
        (legacy_txid, vout, value, script)
        for vout, (value, script) in enumerate(legacy_outputs)
    )
    leaves = [
        hash_utxo(txid_bytes, vout, value, script, 0, GENESIS_TIMESTAMP)
        for txid_bytes, vout, value, script in sorted(utxos, key=lambda u: (u[0], u[1]))
    ]
    utxo_root = merkle_root(leaves)

    header = bincode_header(
        version=1,
        prev_hash=b"\x00" * 32,
        merkle=merkle,
        utxo_root=utxo_root,
        timestamp=GENESIS_TIMESTAMP,
        bits=GENESIS_BITS,
        nonce=0,
        stake_modifier=0,
    )
    genesis_hash = sha256d(header)

    print(f"coinbase txid: {coinbase_txid.hex()}")
    print(f"legacy txid:   {legacy_txid.hex()}")
    print(f"merkle root:   {merkle.hex()}")
    print(f"utxo root:     {utxo_root.hex()}")
    print(f"genesis hash:  {genesis_hash.hex()}")

    if utxo_root != EXPECTED_UTXO_ROOT:
        print(f"FAIL: expected UTXO root {EXPECTED_UTXO_ROOT.hex()}", file=sys.stderr)
        return 1
    if genesis_hash != EXPECTED_GENESIS_HASH:
        print(f"FAIL: expected genesis hash {EXPECTED_GENESIS_HASH.hex()}", file=sys.stderr)
        return 1
    print("PASS: independent genesis hash and UTXO root match")
    return 0


if __name__ == "__main__":
    sys.exit(main())
