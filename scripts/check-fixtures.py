#!/usr/bin/env python3
"""Check synthetic protocol fixtures and standard IMA ADPCM tables offline."""

import hashlib
import hmac
import json
import struct
from pathlib import Path


FIXTURES = Path(__file__).resolve().parent.parent / "tests" / "fixtures"


def main():
    vectors = json.loads((FIXTURES / "protocol-v2-vectors.json").read_text())
    tables = json.loads((FIXTURES / "ima-adpcm-tables.json").read_text())
    messages = vectors["messages"]
    assert {item["type"] for item in messages} == set(range(1, 17))
    by_name = {item["name"]: item for item in messages}
    assert len(by_name) == len(messages)
    for item in messages:
        payload = bytes.fromhex(item["payload_hex"])
        message = bytes.fromhex(item["message_hex"])
        assert message[0] == item["type"], item["name"]
        assert struct.unpack_from("<I", message, 1)[0] == len(payload), item["name"]
        assert message[5:] == payload, item["name"]
        assert len(payload) <= 0x04000000

    auth = vectors["authentication"]
    key = hashlib.pbkdf2_hmac(
        "sha256", auth["password"].encode(), auth["salt_utf8"].encode(),
        auth["iterations"], 32,
    )
    assert key.hex() == auth["derived_key_hex"]
    server = bytes.fromhex(auth["server_nonce_hex"])
    client = bytes.fromhex(auth["client_nonce_hex"])
    assert len(server) == len(client) == 16
    for role in ("client", "server"):
        proof = hmac.digest(key, ("RSCR-" + role).encode() + server + client, "sha256")
        assert proof.hex() == auth[role + "_proof_hex"]
    hello = bytes.fromhex(by_name["hello_password"]["payload_hex"])
    assert hello == b"RSCR" + struct.pack("<HH", 2, 1) + server
    auth_payload = bytes.fromhex(by_name["auth_all_codecs"]["payload_hex"])
    assert auth_payload == client + bytes.fromhex(auth["client_proof_hex"]) + b"\x1f\x01"
    assert len(auth_payload) == 50

    for item in messages:
        payload = bytes.fromhex(item["payload_hex"])
        expected = item["expected"]
        if item["type"] == 12:
            assert payload[0] <= 64
            assert len(payload) == 1 + 8 * payload[0]
            assert all(payload[1 + 8 * i] < 10 for i in range(payload[0]))
        elif item["type"] == 14:
            rate, channels, codec = struct.unpack("<IBB", payload)
            assert (rate, channels, codec) == (
                expected["sample_rate"], expected["channels"], expected["codec"],
            )
        elif item["type"] == 15:
            index, frames = struct.unpack_from("<QH", payload)
            assert index == expected["first_sample_index"]
            assert frames == expected["frames"]
            assert len(expected["samples_i16"]) == frames * expected["channels"]
            assert expected["samples_f32"] == [s / 32768 for s in expected["samples_i16"]]
            if expected["codec"] == 0:
                assert list(struct.unpack("<" + "h" * (frames * expected["channels"]), payload[10:])) == expected["samples_i16"]

    for case in vectors["pixel_cases"]:
        if "drop_bits" in case:
            bits = case["channel_bits"]
            drop = min(case["drop_bits"], bits - 1)
            value = case["input_channel"]
            expanded = value | (value >> (bits - drop)) if drop else value
            assert expanded == case["expanded_channel"]

    assert tables["index"] == [-1, -1, -1, -1, 2, 4, 6, 8]
    steps = tables["step"]
    assert len(steps) == 89
    assert steps[0] == 7 and steps[-1] == 32767
    assert all(a < b for a, b in zip(steps, steps[1:]))
    print(f"Checked {len(messages)} synthetic messages and standard IMA ADPCM tables.")


if __name__ == "__main__":
    main()
