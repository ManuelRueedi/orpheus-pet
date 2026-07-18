"""Materialize the pinned SNAC decoder in a portable local model directory.

This is a release-build helper, not an end-user downloader.  The frozen
backend starts with Hugging Face in offline mode, so a runtime pack is only
publishable when both decoder files are present and match these hashes.
"""

from __future__ import annotations

import argparse
import hashlib
import json
from pathlib import Path
import shutil


REPO_ID = "hubertsiuzdak/snac_24khz"
REVISION = "d73ad176a12188fcf4f360ba3bf2c2fbbe8f58ec"
FILES = {
    "config.json": {
        "byte_size": 300,
        "sha256": "e119b9366d4f5e73c6ca5f31137c4ff361578bbb132953a5203afe037c4012be",
    },
    "pytorch_model.bin": {
        "byte_size": 79_488_254,
        "sha256": "4b8164cc6606bfa627f1a784734c1e539891518f1191ed9194fe1e3b9b4bff40",
    },
}


def _sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def _verify(path: Path, filename: str) -> None:
    expected = FILES[filename]
    actual_size = path.stat().st_size
    if actual_size != expected["byte_size"]:
        raise RuntimeError(
            f"{filename} has {actual_size} bytes; expected {expected['byte_size']}"
        )
    actual_hash = _sha256(path)
    if actual_hash != expected["sha256"]:
        raise RuntimeError(
            f"{filename} SHA-256 is {actual_hash}; expected {expected['sha256']}"
        )


def materialize(model_dir: Path) -> dict[str, object]:
    from huggingface_hub import hf_hub_download

    if model_dir.exists() and any(model_dir.iterdir()):
        raise RuntimeError(f"destination must be empty: {model_dir}")
    model_dir.mkdir(parents=True, exist_ok=True)

    # Copy regular files out of the builder's cache. Hugging Face pointers can
    # be symlinks on developer machines; release archives intentionally contain
    # no links or reparse points. Immutable revision + fixed hashes make reuse
    # of a previously downloaded build asset safe.
    for filename in FILES:
        downloaded = Path(
            hf_hub_download(
                repo_id=REPO_ID,
                filename=filename,
                revision=REVISION,
            )
        )
        _verify(downloaded, filename)
        target = model_dir / filename
        shutil.copyfile(downloaded, target)
        _verify(target, filename)

    # This exercises SNAC's directory-loading branch, including config parsing,
    # checkpoint deserialization, and state-dict compatibility. It never calls
    # Hugging Face because repo_id is an existing directory.
    from snac import SNAC

    SNAC.from_pretrained(str(model_dir)).eval()

    metadata = {
        "schemaVersion": 1,
        "repoId": REPO_ID,
        "revision": REVISION,
        "license": "MIT",
        "files": [
            {
                "path": filename,
                "byteSize": details["byte_size"],
                "sha256": details["sha256"],
            }
            for filename, details in FILES.items()
        ],
    }
    (model_dir / "orpheus-snac.json").write_text(
        json.dumps(metadata, indent=2) + "\n", encoding="utf-8", newline="\n"
    )
    return metadata


def main() -> None:
    parser = argparse.ArgumentParser(
        description="Prefetch the pinned SNAC decoder for an Orpheus runtime pack"
    )
    parser.add_argument("--model-dir", type=Path, required=True)
    options = parser.parse_args()
    metadata = materialize(options.model_dir.resolve())
    print(json.dumps(metadata, separators=(",", ":")))


if __name__ == "__main__":
    main()
