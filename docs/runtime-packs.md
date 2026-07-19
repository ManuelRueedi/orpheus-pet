# Windows runtime packs

Runtime packs are the bridge between the small Orpheus Pet installer and the
large local speech stack. They contain executable dependencies and the fixed
SNAC audio decoder, but never voice GGUF models or machine-local configuration.

## Who uses what

- **End users** will install `Orpheus-Pet-Setup.exe` from a published release
  without Python, Node, pnpm, Rust, Git, or this build script. On first launch,
  the panel asks for consent, downloads the matching CPU/CUDA pack, verifies and
  activates it, and restarts the managed speech services. The release assets
  themselves are not published yet, so the current usable path remains building
  from source until a release maintainer uploads them.
- **Contributors and release builders** use `setup.ps1` and the commands below.
  `setup.ps1` is a source-development bootstrap, not an end-user installer.

## Publish a Windows release

The release workflow is intentionally the normal publishing path. Keep the
version identical in `orpheus-pet/package.json`, `orpheus-pet/src-tauri/Cargo.toml`,
and `orpheus-pet/src-tauri/tauri.conf.json`, then push a stable tag such as
`v0.1.0`. `.github/workflows/release-windows.yml` builds both runtime flavors and
one NSIS installer from that exact tag, validates the complete sidecar-bound
asset set, uploads it to an unpublished draft, verifies GitHub's remote sizes
and SHA-256 digests, and only then publishes it as Latest. A manual dispatch can
rebuild an existing unpublished tag; it will not replace an already-published
release.

The workflow pins its third-party actions, Torch build, CUDA line, and
llama.cpp release. Update those pins as reviewed dependency changes, not as part
of an unrelated app release. The generated installer is not code-signed until a
Windows signing identity is configured; unsigned test releases can therefore
trigger Microsoft Defender SmartScreen.

CI passes `-ReleaseAssetsOnly` to the pack builder so the standard Windows
runner does not retain duplicate unpacked trees. Omit that switch for local
development builds when you also want the inspectable `runtime/<version>/<flavor>`
staging directory.

## Build a pack

Run on Windows. The source environment must already contain the intended
CPU-only or CUDA PyTorch build, and `llama/` must contain the matching Windows
llama.cpp release.

```powershell
# Once per build environment (pinned build-only dependency):
.\Orpheus-FastAPI\venv\Scripts\python.exe -m pip install `
  -r .\scripts\runtime-pack.requirements.txt

# CUDA pack; use -Flavor cpu with a CPU-only setup.ps1 environment.
.\scripts\build-runtime-pack.ps1 -Version 0.1.0 -Flavor cuda
```

The build needs network access once to fetch the pinned SNAC decoder revision;
end-user startup does not. Existing output is never overwritten unless `-Force`
is supplied. A CI job may pass `-BackendOnedir` to reuse an already-built
PyInstaller one-folder backend;
that directory must include the generated `orpheus-runtime-build.json` metadata
with `snacRuntimeContract: 1`, so an older frozen backend that can silently
download the decoder cannot be reused. The script checks PE/Python architecture,
Torch flavor, required llama DLLs, backend assets, the pinned decoder hashes,
ZIP contents, and the absence of voice models or machine-state files before
publishing.

## Layout and integrity

```text
artifacts/runtime-packs/
├─ runtime/<version>/<flavor>/       staged, unpacked payload
│  ├─ manifest.json
│  ├─ llama/
│  │  ├─ llama-server.exe
│  │  └─ *.dll
│  └─ backend/
│     ├─ orpheus-backend.exe
│     ├─ <PyInstaller onedir dependencies>
│     ├─ .env.example
│     ├─ LICENSE-SNAC.txt
│     ├─ snac-model/
│     │  ├─ config.json
│     │  ├─ pytorch_model.bin
│     │  └─ orpheus-snac.json
│     ├─ outputs/
│     ├─ static/
│     └─ templates/
├─ orpheus-runtime-<version>-windows-<arch>-<flavor>.zip          small packs only
├─ orpheus-runtime-<version>-windows-<arch>-<flavor>.zip.part001 oversized packs only
├─ orpheus-runtime-<version>-windows-<arch>-<flavor>.zip.part002 (and so on)
├─ orpheus-runtime-<version>-windows-<arch>-<flavor>.manifest.json
└─ orpheus-runtime-windows-<arch>-<flavor>.manifest.json  stable feed alias
```

The manifest inside the pack records every payload file, its byte size and
SHA-256, plus a deterministic hash of the ordinally sorted file list. The
sidecar manifest adds the ZIP byte size and SHA-256 so a downloader can reject a
corrupt archive before extraction. When that ZIP is larger than 1,992,294,400
bytes (1900 MiB), the builder splits it into deterministic byte ranges no larger
than that limit. Their exact names are the full ZIP name followed by a
three-digit ordinal: `.zip.part001`, `.zip.part002`, and so on. Both the
versioned and stable sidecars then include `archive.parts` in concatenation
order; every part records its own `fileName`, `byteSize`, and `sha256`.
`archive.fileName`, `archive.byteSize`, and `archive.sha256` continue to describe
the complete reconstructed ZIP. Packs at or below the limit omit `parts`, so
their sidecar schema and single-ZIP download remain backward compatible.

Paths in both manifests use `/` and are relative to the version/flavor
directory. Its top-level `llamaServer`, `backendExe`, `backendDir`, and
`backendArgs` fields are the app launch contract.

The build updates the unversioned sidecar alias atomically only after the
versioned archive assets and manifest are complete. Release publishing must
upload that alias together with either its referenced versioned ZIP or every
entry in `archive.parts`. On x64 PCs the app selects the CUDA alias when
`nvidia-smi` finds an R580-or-newer driver (the CUDA 13 runtime baseline) and
the CPU alias otherwise;
`ORPHEUS_PET_RUNTIME_FLAVOR=cpu|cuda` is an explicit override. A custom trusted
sidecar can be supplied at process level with
`ORPHEUS_PET_RUNTIME_MANIFEST_URL` (and is required to exercise the downloader
from a debug build).

For each app release, build both desired flavors. For a pack at or below 1900
MiB, upload the single ZIP. For a larger pack, the builder removes the oversized
whole ZIP after hashing and splitting it; upload **every** `.partNNN` named in
the stable manifest, in addition to the stable manifest itself. Do not recreate
or upload the oversized ZIP: GitHub requires every individual release asset to
be smaller than 2 GiB, and the app reconstructs and verifies it locally.

A release may therefore contain a mix of single and multipart flavors:

```text
orpheus-runtime-<version>-windows-x64-cpu.zip              single-pack example
orpheus-runtime-<version>-windows-x64-cuda.zip.part001     multipart example
orpheus-runtime-<version>-windows-x64-cuda.zip.part002
... one asset for every cuda archive.parts entry
orpheus-runtime-windows-x64-cpu.manifest.json
orpheus-runtime-windows-x64-cuda.manifest.json
Orpheus-Pet-Setup.exe
```

The stable manifests name their exact versioned ZIP, size, and SHA-256 and, for
multipart packs, every ordered part with its own size and SHA-256. The app
requires the ZIP or all parts to resolve from the same HTTPS origin, downloads
each asset with a strict size ceiling, verifies parts before concatenating them,
then verifies the reconstructed ZIP. It rejects unsafe ZIP
paths/links/collisions, verifies every payload hash, and only then swaps
`runtime/current`. A previous active runtime is retained until both expected
HTTP endpoints become ready, and is restored if startup fails. The rollback
directory is also a durable transaction marker: a launch interrupted by a crash
or power loss is recovered before services start.
The first-run consent screen is bound to that manifest's version, flavor,
archive size, and SHA-256. If the Latest feed changes between displaying the
plan and starting the download, the app asks the user to review the new plan.

PyInstaller is deliberately run in **one-folder** mode. A Windows venv is not a
portable runtime, and one-file mode would unpack this multi-gigabyte backend on
every launch. The backend entry point changes to its own directory before
loading `app.py`, so `.env`, `static/`, `templates/`, and `outputs/` do not depend
on a developer checkout or launch directory.

No `*.gguf`, `*.safetensors`, `*.pt`, `*.pth`, `*.ckpt`, or unapproved model
`*.bin` is allowed into a pack. The single exception is the exact, hash-pinned
SNAC `pytorch_model.bin` path above. `modelsIncluded: false` and
`voiceModelsIncluded: false` refer to the separately downloaded voice GGUF
weights; `decoderAssets` records the included decoder repository, immutable
revision, paths, hashes, byte size, and license.

The SNAC snapshot contains a 300-byte config and a 79,488,254-byte weight file
(about 75.8 MiB before ZIP compression). It is pinned to commit
`d73ad176a12188fcf4f360ba3bf2c2fbbe8f58ec`, and both files have fixed SHA-256
checks in `scripts/prefetch_snac.py`. The frozen launcher points
`ORPHEUS_SNAC_MODEL` at this version-local directory and forces Hugging Face
offline before importing the backend. The app launcher sets the same absolute
path when it starts a release pack. `SNAC.from_pretrained` therefore takes its
direct local-directory branch and fails clearly if an incomplete pack somehow
reaches startup; it cannot turn first launch into an invisible download. This
plain directory is intentional: it avoids publishing Hugging Face cache
symlinks/reparse points and remains portable after extraction or activation.

SNAC code and the `hubertsiuzdak/snac_24khz` model are published under the MIT
license. `backend/LICENSE-SNAC.txt` carries the upstream copyright and license
notice in every pack. Release engineering should still treat any future decoder
revision as an explicit dependency update: verify its license, size, hashes,
and audio compatibility before changing the pin. Upstream publishes this
checkpoint as a PyTorch `pytorch_model.bin`, not safetensors; the build accepts
only the pinned SHA-256 and immediately deserializes it to verify SNAC state-dict
compatibility. A pin change therefore also requires a supply-chain review.

The archive is versioned as `runtime/<version>/<flavor>`. The app launches
`runtime/current/manifest.json`; its downloader retains a verified versioned
copy, creates a verified activation candidate, and swaps that directory into
`current` with rollback. Extracting the ZIP alone does not activate it.
