"""Persist the node login once, preserving preferences and pairing state."""
import hashlib
import json
import os
from pathlib import Path
import re
import secrets
import tempfile
import tomllib


def atomic_write(path, content):
    path.parent.mkdir(parents=True, exist_ok=True, mode=0o700)
    fd, tmp = tempfile.mkstemp(dir=path.parent, prefix=path.name + ".")
    try:
        with os.fdopen(fd, "w") as out:
            out.write(content)
            out.flush()
            os.fsync(out.fileno())
        os.replace(tmp, path)
    finally:
        if os.path.exists(tmp):
            os.unlink(tmp)


def configure(host, credentials, environ):
    text = host.read_text() if host.exists() else "power_allowed = true\nstart_with_windows = true\nstay_awake = true\n"
    cfg = tomllib.loads(text)  # Do not overwrite a damaged or unreadable config.
    user = environ.get("BROLINK_USER") or cfg.get("sunshine_user") or "brolink"
    password = environ.get("BROLINK_PASS") or cfg.get("sunshine_pass") or secrets.token_urlsafe(24)
    for key, value in (("sunshine_user", user), ("sunshine_pass", password)):
        line = key + " = " + json.dumps(value) + "\n"
        pattern = re.compile(r"(?m)^" + key + r"\s*=.*(?:\n|$)")
        if pattern.search(text):
            text = pattern.sub(lambda _: line, text)
        else:
            # Root fields must precede any TOML tables.
            text = line + text
    tomllib.loads(text)
    # Commit the source first so a crash between writes reuses this password.
    atomic_write(host, text)
    salt = secrets.token_hex(16)
    digest = hashlib.sha256((password + salt).encode()).digest()[::-1].hex().upper()
    atomic_write(credentials, json.dumps({"username": user, "salt": salt, "password": digest}))


if __name__ == "__main__":
    configure(Path("/root/.local/share/brolink/host.toml"),
              Path("/root/.config/sunshine/brolink-web.json"), os.environ)
