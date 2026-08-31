from __future__ import annotations

import base64
import json
import os
from pathlib import Path

from cryptography.hazmat.primitives.ciphers.aead import AESGCM
from cryptography.hazmat.primitives.kdf.scrypt import Scrypt

AAD = b"local-desensitization-v1"
MIN_PASSWORD_LENGTH = 12
MAX_BLOB_BYTES = 600 * 1024 * 1024
MAX_PAYLOAD_BYTES = 500 * 1024 * 1024


def validate_password(password: str, *, minimum: int = MIN_PASSWORD_LENGTH) -> None:
    if not isinstance(password, str) or len(password) < minimum:
        raise ValueError(f"可逆模式口令至少需要 {minimum} 个字符")
    if any(ord(char) < 32 for char in password):
        raise ValueError("口令包含不可用控制字符")


def _key(password: str, salt: bytes) -> bytes:
    validate_password(password)
    return Scrypt(salt=salt, length=32, n=16384, r=8, p=1).derive(password.encode("utf-8"))


def encrypt_json(payload: dict, password: str) -> bytes:
    validate_password(password)
    if not isinstance(payload, dict):
        raise ValueError("加密载荷必须是对象")
    salt = os.urandom(16)
    nonce = os.urandom(12)
    data = json.dumps(payload, ensure_ascii=False, separators=(",", ":")).encode("utf-8")
    if len(data) > MAX_PAYLOAD_BYTES:
        raise ValueError("加密载荷过大")
    encrypted = AESGCM(_key(password, salt)).encrypt(nonce, data, AAD)
    envelope = {"version": 1, "salt": base64.b64encode(salt).decode(), "nonce": base64.b64encode(nonce).decode(), "ciphertext": base64.b64encode(encrypted).decode()}
    blob = json.dumps(envelope, separators=(",", ":")).encode("ascii")
    if len(blob) > MAX_BLOB_BYTES:
        raise ValueError("加密结果过大")
    return blob


def decrypt_json(blob: bytes, password: str) -> dict:
    validate_password(password)
    if not isinstance(blob, (bytes, bytearray)) or len(blob) > MAX_BLOB_BYTES:
        raise ValueError("加密数据无效或过大")
    try:
        envelope = json.loads(bytes(blob).decode("ascii"))
        if not isinstance(envelope, dict) or envelope.get("version") != 1:
            raise ValueError("不支持的加密版本")
        salt = base64.b64decode(envelope["salt"], validate=True)
        nonce = base64.b64decode(envelope["nonce"], validate=True)
        ciphertext = base64.b64decode(envelope["ciphertext"], validate=True)
        if len(salt) != 16 or len(nonce) != 12 or len(ciphertext) < 16:
            raise ValueError("加密字段长度无效")
        data = AESGCM(_key(password, salt)).decrypt(nonce, ciphertext, AAD)
        if len(data) > MAX_PAYLOAD_BYTES:
            raise ValueError("解密载荷过大")
        payload = json.loads(data.decode("utf-8"))
    except ValueError:
        raise
    except (KeyError, TypeError, UnicodeError, json.JSONDecodeError) as exc:
        raise ValueError("加密数据格式无效") from exc
    if not isinstance(payload, dict):
        raise ValueError("解密载荷格式无效")
    return payload


def encrypt_file(path: Path, payload: dict, password: str) -> None:
    path.write_bytes(encrypt_json(payload, password))
