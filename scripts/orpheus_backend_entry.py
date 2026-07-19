"""Frozen entry point for the distributable Orpheus backend.

The development server is normally launched as ``python -m uvicorn app:app``.
Release runtime packs instead freeze this small wrapper with PyInstaller so an
end user does not need a system Python installation or a relocatable venv.
"""

from __future__ import annotations

import argparse
import os
from pathlib import Path
import sys


SNAC_REVISION = "d73ad176a12188fcf4f360ba3bf2c2fbbe8f58ec"


def _configure_utf8_console() -> None:
    """Keep frozen startup logs safe on Windows' legacy console encodings."""
    for stream in (sys.stdout, sys.stderr):
        reconfigure = getattr(stream, "reconfigure", None)
        if callable(reconfigure):
            reconfigure(encoding="utf-8", errors="replace")


def _backend_directory() -> Path:
    if getattr(sys, "frozen", False):
        return Path(sys.executable).resolve().parent
    return Path(__file__).resolve().parents[1] / "Orpheus-FastAPI"


def _upsert_env(path: Path, key: str, value: str) -> None:
    """Persist a launcher override because app.py loads .env with override=True."""
    lines = path.read_text(encoding="utf-8").splitlines() if path.is_file() else []
    replacement = f"{key}={value}"
    found = False
    updated: list[str] = []
    for line in lines:
        if not found and line.lstrip().startswith(f"{key}="):
            updated.append(replacement)
            found = True
        else:
            updated.append(line)
    if not found:
        updated.append(replacement)
    path.write_text("\n".join(updated) + "\n", encoding="utf-8")


def _port(value: str) -> int:
    port = int(value)
    if not 1 <= port <= 65535:
        raise argparse.ArgumentTypeError("port must be between 1 and 65535")
    return port


def _configure_packaged_snac(backend_directory: Path) -> None:
    """Make the frozen backend's decoder lookup deterministic and offline."""
    if not getattr(sys, "frozen", False):
        return

    model_directory = backend_directory / "snac-model"
    missing = [
        str(path)
        for path in (
            model_directory / "config.json",
            model_directory / "pytorch_model.bin",
        )
        if not path.is_file()
    ]
    if missing:
        raise RuntimeError(
            "The runtime pack is missing its SNAC decoder assets: " + ", ".join(missing)
        )

    # The release model directory is immutable, version-local, and
    # manifest-verified. Force offline mode as defense-in-depth even though
    # SNAC loads an existing local directory without calling Hugging Face.
    os.environ["HF_HUB_OFFLINE"] = "1"
    os.environ["ORPHEUS_SNAC_MODEL"] = str(model_directory)
    os.environ["ORPHEUS_SNAC_REVISION"] = SNAC_REVISION
    os.environ["ORPHEUS_SNAC_LOCAL_ONLY"] = "1"


def main() -> None:
    # PyInstaller initializes its embedded interpreter before PYTHONUTF8 can
    # affect stdio. app.py and the vendored engine log Unicode status symbols,
    # so configure both streams before importing either module.
    _configure_utf8_console()

    parser = argparse.ArgumentParser(description="Run the packaged Orpheus TTS backend")
    parser.add_argument("--host", default=os.environ.get("ORPHEUS_HOST", "127.0.0.1"))
    parser.add_argument(
        "--port",
        type=_port,
        default=_port(os.environ.get("ORPHEUS_PORT", "5005")),
    )
    parser.add_argument(
        "--llama-url",
        default=None,
        help="llama.cpp completions endpoint; persisted to the runtime .env",
    )
    options = parser.parse_args()

    backend_directory = _backend_directory()
    os.chdir(backend_directory)
    sys.path.insert(0, str(backend_directory))
    _configure_packaged_snac(backend_directory)

    os.environ["ORPHEUS_HOST"] = options.host
    os.environ["ORPHEUS_PORT"] = str(options.port)
    if options.llama_url:
        os.environ["ORPHEUS_API_URL"] = options.llama_url
        _upsert_env(backend_directory / ".env", "ORPHEUS_API_URL", options.llama_url)

    # Imported only after the working directory and environment are ready:
    # app.py resolves static/, templates/, outputs/, and .env relative to cwd.
    import uvicorn
    from app import app

    uvicorn.run(app, host=options.host, port=options.port, reload=False)


if __name__ == "__main__":
    main()
