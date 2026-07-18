#Requires -Version 5.1
<#
.SYNOPSIS
    Build a versioned Windows runtime pack for Orpheus Pet.

.DESCRIPTION
    Freezes Orpheus-FastAPI as a PyInstaller one-folder executable, copies the
    minimum llama.cpp server runtime, prefetched SNAC decoder assets, and emits
    both a staged runtime tree and a ZIP. Large voice models and machine-local
    configuration are deliberately excluded.

    Run this on Windows with the same Python environment and llama.cpp flavor
    that will be shipped. A Python venv is used only as build input; it is never
    copied into the runtime pack.

.PARAMETER Version
    Runtime version written into the path and manifest, for example 0.1.0.

.PARAMETER Flavor
    cpu or cuda. The Python/Torch and llama.cpp inputs are validated against it.

.PARAMETER BackendOnedir
    Optional prebuilt PyInstaller one-folder directory. Intended for CI reuse;
    when omitted, the backend is built from Orpheus-FastAPI/venv.

.EXAMPLE
    .\scripts\build-runtime-pack.ps1 -Version 0.1.0 -Flavor cuda

.EXAMPLE
    .\scripts\build-runtime-pack.ps1 -Version 0.1.0 -Flavor cpu -Force
#>
[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [ValidatePattern('^[0-9A-Za-z][0-9A-Za-z.+-]{0,63}$')]
    [string] $Version,

    [ValidateSet('cpu', 'cuda')]
    [string] $Flavor = 'cuda',

    [ValidateSet('x64', 'arm64')]
    [string] $Architecture = 'x64',

    [string] $RepositoryRoot,
    [string] $PythonPath,
    [string] $LlamaDirectory,
    [string] $BackendSource,
    [string] $BackendOnedir,
    [string] $OutputRoot,
    [switch] $Force
)

$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'

function Write-Step {
    param([string] $Message)
    Write-Host "`n==> $Message" -ForegroundColor Cyan
}

function Assert-File {
    param([string] $Path, [string] $Description)
    if (-not (Test-Path -LiteralPath $Path -PathType Leaf)) {
        throw "Missing $Description`: $Path"
    }
}

function Assert-Directory {
    param([string] $Path, [string] $Description)
    if (-not (Test-Path -LiteralPath $Path -PathType Container)) {
        throw "Missing $Description`: $Path"
    }
}

function Assert-NoReparsePoints {
    param([string] $Path, [string] $Description)
    $root = Get-Item -LiteralPath $Path -Force
    if (($root.Attributes -band [System.IO.FileAttributes]::ReparsePoint) -ne 0) {
        throw "$Description is a link/reparse point: $($root.FullName)"
    }

    $items = @()
    if ($root.PSIsContainer) {
        $items += @(Get-ChildItem -LiteralPath $Path -Recurse -Force)
    }
    $points = @($items | Where-Object {
        ($_.Attributes -band [System.IO.FileAttributes]::ReparsePoint) -ne 0
    })
    if ($points.Count -gt 0) {
        throw "$Description contains links/reparse points: $($points.FullName -join ', ')"
    }
}

function Assert-NoReparseAncestors {
    param([string] $Path, [string] $Description)
    $current = [System.IO.DirectoryInfo] [System.IO.Path]::GetFullPath($Path)
    while ($null -ne $current) {
        if ($current.Exists -and
                (($current.Attributes -band [System.IO.FileAttributes]::ReparsePoint) -ne 0)) {
            throw "$Description uses a link/reparse-point ancestor: $($current.FullName)"
        }
        $current = $current.Parent
    }
}

function Get-PortableRelativePath {
    param([string] $BasePath, [string] $FilePath)
    $base = [System.IO.Path]::GetFullPath($BasePath).TrimEnd('\', '/')
    $file = [System.IO.Path]::GetFullPath($FilePath)
    if (-not $file.StartsWith($base + [System.IO.Path]::DirectorySeparatorChar,
            [System.StringComparison]::OrdinalIgnoreCase)) {
        throw "Path is outside payload root: $file"
    }
    return $file.Substring($base.Length + 1).Replace('\', '/')
}

function Get-BytesSha256 {
    param([byte[]] $Bytes)
    $algorithm = [System.Security.Cryptography.SHA256]::Create()
    try {
        return (($algorithm.ComputeHash($Bytes) | ForEach-Object { $_.ToString('x2') }) -join '')
    } finally {
        $algorithm.Dispose()
    }
}

function Write-Utf8NoBom {
    param([string] $Path, [string] $Content)
    $encoding = New-Object System.Text.UTF8Encoding($false)
    [System.IO.File]::WriteAllText($Path, $Content, $encoding)
}

function Write-Utf8NoBomAtomic {
    param([string] $Path, [string] $Content)
    $fullPath = [System.IO.Path]::GetFullPath($Path)
    $directory = [System.IO.Path]::GetDirectoryName($fullPath)
    $temporary = Join-Path $directory ('.manifest-' + [guid]::NewGuid().ToString('N') + '.tmp')
    $backup = Join-Path $directory ('.manifest-' + [guid]::NewGuid().ToString('N') + '.bak')
    try {
        Write-Utf8NoBom -Path $temporary -Content $Content
        if (Test-Path -LiteralPath $fullPath -PathType Leaf) {
            [System.IO.File]::Replace($temporary, $fullPath, $backup, $true)
            if (Test-Path -LiteralPath $backup) { Remove-Item -LiteralPath $backup -Force }
        } else {
            [System.IO.File]::Move($temporary, $fullPath)
        }
    } finally {
        if (Test-Path -LiteralPath $temporary) { Remove-Item -LiteralPath $temporary -Force }
        if (Test-Path -LiteralPath $backup) { Remove-Item -LiteralPath $backup -Force }
    }
}

function Test-PathWithin {
    param([string] $ChildPath, [string] $ParentPath)
    $child = [System.IO.Path]::GetFullPath($ChildPath).TrimEnd('\', '/')
    $parent = [System.IO.Path]::GetFullPath($ParentPath).TrimEnd('\', '/')
    if ($child.Equals($parent, [System.StringComparison]::OrdinalIgnoreCase)) { return $true }
    return $child.StartsWith(
        $parent + [System.IO.Path]::DirectorySeparatorChar,
        [System.StringComparison]::OrdinalIgnoreCase
    )
}

function Get-PeArchitecture {
    param([string] $Path)
    $stream = [System.IO.File]::Open($Path, [System.IO.FileMode]::Open,
        [System.IO.FileAccess]::Read, [System.IO.FileShare]::Read)
    $reader = New-Object System.IO.BinaryReader($stream)
    try {
        if ($reader.ReadUInt16() -ne 0x5A4D) { throw "Not a PE executable: $Path" }
        $stream.Position = 0x3C
        $peOffset = $reader.ReadInt32()
        if ($peOffset -lt 0 -or $peOffset -gt ($stream.Length - 6)) {
            throw "Invalid PE header: $Path"
        }
        $stream.Position = $peOffset
        if ($reader.ReadUInt32() -ne 0x00004550) { throw "Invalid PE signature: $Path" }
        switch ($reader.ReadUInt16()) {
            0x8664 { return 'x64' }
            0xAA64 { return 'arm64' }
            0x014C { return 'x86' }
            default { return 'unknown' }
        }
    } finally {
        $reader.Dispose()
        $stream.Dispose()
    }
}

function Assert-PeArchitecture {
    param([string] $Path, [string] $Expected)
    $actual = Get-PeArchitecture -Path $Path
    if ($actual -ne $Expected) {
        throw "Architecture mismatch for $Path`: expected $Expected, found $actual"
    }
}

$scriptRoot = [System.IO.Path]::GetFullPath($PSScriptRoot)
if ([string]::IsNullOrWhiteSpace($RepositoryRoot)) {
    $RepositoryRoot = Join-Path $scriptRoot '..'
}
$RepositoryRoot = [System.IO.Path]::GetFullPath($RepositoryRoot)

if ([string]::IsNullOrWhiteSpace($BackendSource)) {
    $BackendSource = Join-Path $RepositoryRoot 'Orpheus-FastAPI'
}
if ([string]::IsNullOrWhiteSpace($LlamaDirectory)) {
    $LlamaDirectory = Join-Path $RepositoryRoot 'llama'
}
if ([string]::IsNullOrWhiteSpace($PythonPath)) {
    $PythonPath = Join-Path $BackendSource 'venv\Scripts\python.exe'
}
if ([string]::IsNullOrWhiteSpace($OutputRoot)) {
    $OutputRoot = Join-Path $RepositoryRoot 'artifacts\runtime-packs'
}

$BackendSource = [System.IO.Path]::GetFullPath($BackendSource)
$LlamaDirectory = [System.IO.Path]::GetFullPath($LlamaDirectory)
$PythonPath = [System.IO.Path]::GetFullPath($PythonPath)
$OutputRoot = [System.IO.Path]::GetFullPath($OutputRoot)
if (-not [string]::IsNullOrWhiteSpace($BackendOnedir)) {
    $BackendOnedir = [System.IO.Path]::GetFullPath($BackendOnedir)
}

Assert-NoReparseAncestors -Path $OutputRoot -Description 'OutputRoot'
Assert-NoReparseAncestors -Path $BackendSource -Description 'BackendSource'
Assert-NoReparseAncestors -Path $LlamaDirectory -Description 'LlamaDirectory'
if (-not [string]::IsNullOrWhiteSpace($BackendOnedir)) {
    Assert-NoReparseAncestors -Path $BackendOnedir -Description 'BackendOnedir'
}

# The work tree lives below OutputRoot. If that is itself inside a recursively
# copied source, the copy can consume its own output or publish partial state.
$recursiveSources = @($BackendSource, $LlamaDirectory)
if (-not [string]::IsNullOrWhiteSpace($BackendOnedir)) {
    $recursiveSources += $BackendOnedir
}
foreach ($source in $recursiveSources) {
    if (Test-PathWithin -ChildPath $OutputRoot -ParentPath $source) {
        throw "OutputRoot must not be inside a copied source directory: $source"
    }
}

$llamaServer = Join-Path $LlamaDirectory 'llama-server.exe'
$backendApp = Join-Path $BackendSource 'app.py'
$backendEngine = Join-Path $BackendSource 'tts_engine\__init__.py'
$entryPoint = Join-Path $scriptRoot 'orpheus_backend_entry.py'
$buildRequirements = Join-Path $scriptRoot 'runtime-pack.requirements.txt'
$snacPrefetch = Join-Path $scriptRoot 'prefetch_snac.py'
$snacLicense = Join-Path $scriptRoot 'licenses\SNAC-MIT.txt'

Write-Step 'Validating source inputs'
Assert-Directory -Path $BackendSource -Description 'Orpheus-FastAPI source directory'
Assert-Directory -Path $LlamaDirectory -Description 'llama.cpp runtime directory'
Assert-File -Path $llamaServer -Description 'llama-server executable'
Assert-File -Path $backendApp -Description 'backend app.py'
Assert-File -Path $backendEngine -Description 'backend tts_engine package'
Assert-Directory -Path (Join-Path $BackendSource 'static') -Description 'backend static directory'
Assert-Directory -Path (Join-Path $BackendSource 'templates') -Description 'backend templates directory'
Assert-File -Path (Join-Path $BackendSource '.env.example') -Description 'backend .env.example'
Assert-File -Path $entryPoint -Description 'frozen backend entry point'
Assert-File -Path $buildRequirements -Description 'runtime-pack build requirements'
Assert-File -Path $snacPrefetch -Description 'SNAC prefetch helper'
Assert-File -Path $snacLicense -Description 'SNAC MIT license'
Assert-File -Path $PythonPath -Description 'backend build Python'
Assert-PeArchitecture -Path $llamaServer -Expected $Architecture
Assert-NoReparsePoints -Path $LlamaDirectory -Description 'llama.cpp runtime input'
Assert-NoReparsePoints -Path (Join-Path $BackendSource 'static') -Description 'backend static assets'
Assert-NoReparsePoints -Path (Join-Path $BackendSource 'templates') -Description 'backend templates'
Assert-NoReparsePoints -Path (Join-Path $BackendSource '.env.example') -Description 'backend environment template'
Assert-NoReparsePoints -Path (Join-Path $BackendSource 'LICENSE') -Description 'backend license'

$llamaDlls = @(Get-ChildItem -LiteralPath $LlamaDirectory -File -Filter '*.dll')
if ($llamaDlls.Count -eq 0) { throw "No llama.cpp DLLs found in $LlamaDirectory" }
if (-not ($llamaDlls | Where-Object { $_.Name -eq 'llama.dll' })) {
    throw "llama.dll is required beside llama-server.exe"
}
foreach ($requiredDll in @(
        'llama-common.dll',
        'llama-server-impl.dll',
        'mtmd.dll',
        'ggml.dll',
        'ggml-base.dll'
    )) {
    if (-not ($llamaDlls | Where-Object { $_.Name -eq $requiredDll })) {
        throw "$requiredDll is required beside llama-server.exe"
    }
}
if (-not ($llamaDlls | Where-Object { $_.Name -like 'ggml-cpu*.dll' })) {
    throw "At least one ggml-cpu*.dll is required beside llama-server.exe"
}
if ($Flavor -eq 'cuda') {
    if (-not ($llamaDlls | Where-Object { $_.Name -eq 'ggml-cuda.dll' })) {
        throw "CUDA flavor requires ggml-cuda.dll"
    }
    if (-not ($llamaDlls | Where-Object { $_.Name -like 'cudart64*.dll' })) {
        throw "CUDA flavor requires a cudart64*.dll"
    }
    if (-not ($llamaDlls | Where-Object { $_.Name -like 'cublas64*.dll' })) {
        throw "CUDA flavor requires a cublas64*.dll"
    }
    if (-not ($llamaDlls | Where-Object { $_.Name -like 'cublasLt64*.dll' })) {
        throw "CUDA flavor requires a cublasLt64*.dll"
    }
}

$packId = "orpheus-runtime-$Version-windows-$Architecture-$Flavor"
$stagingPath = Join-Path $OutputRoot "runtime\$Version\$Flavor"
$archivePath = Join-Path $OutputRoot "$packId.zip"
$archiveManifestPath = Join-Path $OutputRoot "$packId.manifest.json"
$stableManifestPath = Join-Path $OutputRoot "orpheus-runtime-windows-$Architecture-$Flavor.manifest.json"

$existingOutputs = @($stagingPath, $archivePath, $archiveManifestPath) |
    Where-Object { Test-Path -LiteralPath $_ }
if ($existingOutputs.Count -gt 0 -and -not $Force) {
    throw "Output already exists. Choose another version/output or pass -Force: $($existingOutputs -join ', ')"
}

New-Item -ItemType Directory -Force -Path $OutputRoot | Out-Null
$workParent = Join-Path $OutputRoot '.work'
New-Item -ItemType Directory -Force -Path $workParent | Out-Null
$workRoot = Join-Path $workParent ($packId + '-' + [guid]::NewGuid().ToString('N'))
$archiveRoot = Join-Path $workRoot 'archive'
$packRoot = Join-Path $archiveRoot "runtime\$Version\$Flavor"
$packBackend = Join-Path $packRoot 'backend'
$packLlama = Join-Path $packRoot 'llama'
New-Item -ItemType Directory -Force -Path $packBackend | Out-Null
New-Item -ItemType Directory -Force -Path $packLlama | Out-Null

try {
    Write-Step 'Building frozen backend'
    $builtBackend = $null
    if (-not [string]::IsNullOrWhiteSpace($BackendOnedir)) {
        Assert-Directory -Path $BackendOnedir -Description 'prebuilt backend one-folder directory'
        Assert-File -Path (Join-Path $BackendOnedir 'orpheus-backend.exe') `
            -Description 'prebuilt orpheus-backend executable'
        $metadataPath = Join-Path $BackendOnedir 'orpheus-runtime-build.json'
        Assert-File -Path $metadataPath -Description 'prebuilt backend build metadata'
        try {
            $backendMetadata = Get-Content -LiteralPath $metadataPath -Raw | ConvertFrom-Json
        } catch {
            throw "Invalid prebuilt backend metadata at $metadataPath`: $($_.Exception.Message)"
        }
        if ($backendMetadata.schemaVersion -ne 1) {
            throw "Unsupported prebuilt backend metadata schema: $($backendMetadata.schemaVersion)"
        }
        if ($backendMetadata.architecture -ne $Architecture) {
            throw "Prebuilt backend architecture is $($backendMetadata.architecture), expected $Architecture"
        }
        if ($backendMetadata.flavor -ne $Flavor) {
            throw "Prebuilt backend flavor is $($backendMetadata.flavor), expected $Flavor"
        }
        if ($backendMetadata.snacRuntimeContract -ne 1) {
            throw 'Prebuilt backend does not guarantee the packaged, offline SNAC runtime contract'
        }
        $builtBackend = $BackendOnedir
    } else {
        $probeCode = @'
import json, platform, struct, torch
print(json.dumps({
    "bits": struct.calcsize("P") * 8,
    "machine": platform.machine(),
    "torch": torch.__version__,
    "torchCuda": torch.version.cuda,
}))
'@
        $probeOutput = (& $PythonPath -c $probeCode | Out-String).Trim()
        if ($LASTEXITCODE -ne 0) { throw "Python/Torch build-environment probe failed" }
        try { $probe = $probeOutput | ConvertFrom-Json } catch {
            throw "Python/Torch build-environment probe returned invalid JSON: $probeOutput"
        }
        $expectedBits = if ($Architecture -eq 'x64' -or $Architecture -eq 'arm64') { 64 } else { 32 }
        if ($probe.bits -ne $expectedBits) {
            throw "Python is $($probe.bits)-bit; $Architecture requires $expectedBits-bit Python"
        }
        if ($Flavor -eq 'cuda' -and [string]::IsNullOrWhiteSpace($probe.torchCuda)) {
            throw "CUDA flavor requires a CUDA-enabled PyTorch build"
        }
        if ($Flavor -eq 'cpu' -and -not [string]::IsNullOrWhiteSpace($probe.torchCuda)) {
            throw "CPU flavor requires a CPU-only PyTorch environment (found CUDA $($probe.torchCuda))"
        }

        $pyInstallerVersion = (& $PythonPath -c 'import PyInstaller; print(PyInstaller.__version__)' |
            Out-String).Trim()
        if ($LASTEXITCODE -ne 0) {
            throw "PyInstaller is missing. Run: `"$PythonPath`" -m pip install -r `"$buildRequirements`""
        }
        if ($pyInstallerVersion -ne '6.21.0') {
            throw "Expected PyInstaller 6.21.0, found $pyInstallerVersion. Install $buildRequirements"
        }

        $pyInstallerDist = Join-Path $workRoot 'pyinstaller-dist'
        $pyInstallerWork = Join-Path $workRoot 'pyinstaller-work'
        $pyInstallerSpec = Join-Path $workRoot 'pyinstaller-spec'
        $pyInstallerArgs = @(
            '-m', 'PyInstaller',
            '--noconfirm',
            '--clean',
            '--onedir',
            '--contents-directory', '.',
            '--noupx',
            '--name', 'orpheus-backend',
            '--paths', $BackendSource,
            '--collect-all', 'snac',
            '--collect-submodules', 'uvicorn',
            '--hidden-import', 'tts_engine.inference',
            '--hidden-import', 'tts_engine.speechpipe',
            '--exclude-module', 'IPython',
            '--exclude-module', 'jupyter',
            '--exclude-module', 'matplotlib',
            '--exclude-module', 'pytest',
            '--exclude-module', 'tkinter',
            '--distpath', $pyInstallerDist,
            '--workpath', $pyInstallerWork,
            '--specpath', $pyInstallerSpec,
            $entryPoint
        )
        & $PythonPath @pyInstallerArgs
        if ($LASTEXITCODE -ne 0) { throw "PyInstaller backend build failed (exit $LASTEXITCODE)" }
        $builtBackend = Join-Path $pyInstallerDist 'orpheus-backend'
        Assert-File -Path (Join-Path $builtBackend 'orpheus-backend.exe') `
            -Description 'built orpheus-backend executable'
        $backendMetadata = [pscustomobject][ordered]@{
            schemaVersion = 1
            architecture = $Architecture
            flavor = $Flavor
            pythonBits = [int] $probe.bits
            pythonMachine = [string] $probe.machine
            torch = [string] $probe.torch
            torchCuda = $probe.torchCuda
            pyInstaller = $pyInstallerVersion
            snacRuntimeContract = 1
        }
        Write-Utf8NoBom -Path (Join-Path $builtBackend 'orpheus-runtime-build.json') `
            -Content ($backendMetadata | ConvertTo-Json -Depth 4)
    }

    Assert-NoReparsePoints -Path $builtBackend -Description 'prebuilt backend'
    $machineState = @(Get-ChildItem -LiteralPath $builtBackend -Recurse -Force -File |
        Where-Object {
            $relative = Get-PortableRelativePath -BasePath $builtBackend -FilePath $_.FullName
            $_.Name -eq '.env' -or
                $relative -match '(^|/)outputs/' -or
                $_.Extension.ToLowerInvariant() -in @(
                    '.log', '.wav', '.mp3', '.flac', '.ogg',
                    '.gguf', '.safetensors', '.pt', '.pth', '.ckpt', '.bin'
                )
        })
    if ($machineState.Count -gt 0) {
        throw "Prebuilt backend contains machine state or model data: $($machineState.FullName -join ', ')"
    }

    Get-ChildItem -LiteralPath $builtBackend -Force | ForEach-Object {
        Copy-Item -LiteralPath $_.FullName -Destination $packBackend -Recurse -Force
    }
    Assert-PeArchitecture -Path (Join-Path $packBackend 'orpheus-backend.exe') `
        -Expected $Architecture

    # app.py resolves these against its working directory. The entry point moves
    # there before importing app; keeping them outside the executable also makes
    # auditing and future updates straightforward.
    Copy-Item -LiteralPath (Join-Path $BackendSource '.env.example') -Destination $packBackend
    Copy-Item -LiteralPath (Join-Path $BackendSource 'static') -Destination $packBackend -Recurse
    Copy-Item -LiteralPath (Join-Path $BackendSource 'templates') -Destination $packBackend -Recurse
    Copy-Item -LiteralPath (Join-Path $BackendSource 'LICENSE') `
        -Destination (Join-Path $packBackend 'LICENSE-Orpheus-FastAPI.txt')
    Copy-Item -LiteralPath $snacLicense `
        -Destination (Join-Path $packBackend 'LICENSE-SNAC.txt')
    New-Item -ItemType Directory -Force -Path (Join-Path $packBackend 'outputs') | Out-Null

    Write-Step 'Prefetching pinned SNAC decoder'
    $packSnacModel = Join-Path $packBackend 'snac-model'
    $snacMetadataOutput = (& $PythonPath $snacPrefetch --model-dir $packSnacModel |
        Out-String).Trim()
    if ($LASTEXITCODE -ne 0) {
        throw "SNAC decoder prefetch failed (exit $LASTEXITCODE)"
    }
    try {
        $snacMetadata = $snacMetadataOutput | ConvertFrom-Json
    } catch {
        throw "SNAC prefetch returned invalid metadata: $snacMetadataOutput"
    }
    if ($snacMetadata.schemaVersion -ne 1 -or
            $snacMetadata.repoId -ne 'hubertsiuzdak/snac_24khz' -or
            $snacMetadata.revision -ne 'd73ad176a12188fcf4f360ba3bf2c2fbbe8f58ec') {
        throw "SNAC prefetch returned unexpected metadata: $snacMetadataOutput"
    }

    Write-Step 'Copying llama.cpp runtime'
    Copy-Item -LiteralPath $llamaServer -Destination $packLlama
    $llamaDllsToCopy = $llamaDlls
    if ($Flavor -eq 'cpu') {
        $llamaDllsToCopy = @($llamaDlls | Where-Object {
            $_.Name -notmatch '^(cublas|cudart|ggml-cuda|cuda)'
        })
    }
    $llamaDllsToCopy | ForEach-Object {
        Copy-Item -LiteralPath $_.FullName -Destination $packLlama
    }

    # Remove only build/debug debris from the newly-created staging tree.
    Get-ChildItem -LiteralPath $packRoot -Recurse -Force -File |
        Where-Object { $_.Extension -in @('.pdb', '.lib', '.exp', '.map', '.pyc', '.pyo') } |
        Remove-Item -Force
    Get-ChildItem -LiteralPath $packRoot -Recurse -Force -Directory |
        Where-Object { $_.Name -in @('__pycache__', '.pytest_cache') } |
        Sort-Object { $_.FullName.Length } -Descending |
        Remove-Item -Recurse -Force

    Write-Step 'Validating staged executables'
    $stagedLlamaServer = Join-Path $packLlama 'llama-server.exe'
    Push-Location $packLlama
    try {
        $llamaVersionOutput = (& $stagedLlamaServer --version 2>&1 | Out-String).Trim()
        $llamaVersionExitCode = $LASTEXITCODE
    } finally {
        Pop-Location
    }
    if ($llamaVersionExitCode -ne 0) {
        throw "Staged llama-server failed to load (exit $llamaVersionExitCode): $llamaVersionOutput"
    }

    Write-Step 'Creating payload manifest'
    $allowedSnacWeight = 'backend/snac-model/pytorch_model.bin'
    $forbiddenVoiceModels = @(Get-ChildItem -LiteralPath $packRoot -Recurse -Force -File |
        Where-Object {
            $relative = Get-PortableRelativePath -BasePath $packRoot -FilePath $_.FullName
            $_.Extension.ToLowerInvariant() -in @(
                '.gguf', '.safetensors', '.pt', '.pth', '.ckpt', '.bin'
            ) -and -not $relative.Equals(
                $allowedSnacWeight,
                [System.StringComparison]::Ordinal
            )
        })
    if ($forbiddenVoiceModels.Count -gt 0) {
        throw "Voice or unapproved model file reached runtime staging: $($forbiddenVoiceModels.FullName -join ', ')"
    }
    if (Test-Path -LiteralPath (Join-Path $packBackend 'pyvenv.cfg')) {
        throw "A Python venv was copied into the runtime pack; package the PyInstaller onedir instead"
    }
    Assert-NoReparsePoints -Path $packRoot -Description 'runtime staging tree'

    [long] $payloadBytes = 0
    $payloadFiles = @(Get-ChildItem -LiteralPath $packRoot -Recurse -Force -File)
    $payloadRecords = New-Object 'System.Collections.Generic.List[object]'
    foreach ($file in $payloadFiles) {
        $relativePath = Get-PortableRelativePath -BasePath $packRoot -FilePath $file.FullName
        $hash = (Get-FileHash -LiteralPath $file.FullName -Algorithm SHA256).Hash.ToLowerInvariant()
        $payloadBytes += $file.Length
        [void] $payloadRecords.Add([pscustomobject][ordered]@{
            path = $relativePath
            byteSize = [long] $file.Length
            sha256 = $hash
        })
    }
    $payloadRecords.Sort([System.Comparison[object]] {
        param($left, $right)
        return [System.StringComparer]::Ordinal.Compare(
            [string] $left.path,
            [string] $right.path
        )
    })
    $payloadRecords = @(
        foreach ($record in $payloadRecords) {
            $record
        }
    )
    $treeText = (($payloadRecords | ForEach-Object { "$($_.sha256)  $($_.path)" }) -join "`n") + "`n"
    $treeHash = Get-BytesSha256 -Bytes ([System.Text.Encoding]::UTF8.GetBytes($treeText))

    $manifest = [pscustomobject][ordered]@{
        schemaVersion = 1
        version = $Version
        platform = 'windows'
        architecture = $Architecture
        flavor = $Flavor
        createdUtc = [DateTime]::UtcNow.ToString('o')
        modelsIncluded = $false
        voiceModelsIncluded = $false
        decoderAssets = [pscustomobject][ordered]@{
            included = $true
            repoId = [string] $snacMetadata.repoId
            revision = [string] $snacMetadata.revision
            license = [string] $snacMetadata.license
            modelRoot = 'backend/snac-model'
            licenseFile = 'backend/LICENSE-SNAC.txt'
            byteSize = [long] (($snacMetadata.files | Measure-Object -Property byteSize -Sum).Sum)
            files = $snacMetadata.files
        }
        llamaServer = 'llama/llama-server.exe'
        backendExe = 'backend/orpheus-backend.exe'
        backendDir = 'backend'
        backendArgs = @()
        executables = [pscustomobject][ordered]@{
            llamaServer = 'llama/llama-server.exe'
            backend = 'backend/orpheus-backend.exe'
        }
        payload = [pscustomobject][ordered]@{
            byteSize = $payloadBytes
            fileCount = $payloadRecords.Count
            sha256 = $treeHash
            hashFormat = 'sha256-lowercase, two spaces, portable path, LF; StringComparer.Ordinal path order'
        }
        files = $payloadRecords
    }
    $manifestPath = Join-Path $packRoot 'manifest.json'
    Write-Utf8NoBom -Path $manifestPath -Content ($manifest | ConvertTo-Json -Depth 8)

    Assert-File -Path (Join-Path $packRoot 'llama\llama-server.exe') `
        -Description 'staged llama-server executable'
    Assert-File -Path (Join-Path $packRoot 'backend\orpheus-backend.exe') `
        -Description 'staged backend executable'
    Assert-File -Path (Join-Path $packRoot $allowedSnacWeight.Replace('/', '\')) `
        -Description 'staged SNAC decoder weights'
    Assert-File -Path (Join-Path $packRoot 'backend\static\favicon.ico') `
        -Description 'staged backend static assets'
    Assert-File -Path (Join-Path $packRoot 'backend\templates\tts.html') `
        -Description 'staged backend templates'

    Write-Step 'Writing staging folder and ZIP'
    if ($Force) {
        if (Test-Path -LiteralPath $stagingPath) { Remove-Item -LiteralPath $stagingPath -Recurse -Force }
        if (Test-Path -LiteralPath $archivePath) { Remove-Item -LiteralPath $archivePath -Force }
        if (Test-Path -LiteralPath $archiveManifestPath) {
            Remove-Item -LiteralPath $archiveManifestPath -Force
        }
    }
    New-Item -ItemType Directory -Force -Path $stagingPath | Out-Null
    Get-ChildItem -LiteralPath $packRoot -Force | ForEach-Object {
        Copy-Item -LiteralPath $_.FullName -Destination $stagingPath -Recurse -Force
    }

    # Compress-Archive on Windows PowerShell 5.1 silently omits Hidden files.
    # ZipArchive enumerates the full source tree; compare its resulting entries
    # with the source so the archive cannot disagree with the payload manifest.
    Add-Type -AssemblyName System.IO.Compression.FileSystem
    [System.IO.Compression.ZipFile]::CreateFromDirectory(
        $archiveRoot,
        $archivePath,
        [System.IO.Compression.CompressionLevel]::Optimal,
        $false
    )
    Assert-File -Path $archivePath -Description 'runtime ZIP'
    $expectedArchiveEntries = @(Get-ChildItem -LiteralPath $archiveRoot -Recurse -Force -File |
        ForEach-Object {
            Get-PortableRelativePath -BasePath $archiveRoot -FilePath $_.FullName
        } | Sort-Object -Unique)
    $zip = [System.IO.Compression.ZipFile]::OpenRead($archivePath)
    try {
        $actualArchiveEntries = @($zip.Entries |
            Where-Object { -not [string]::IsNullOrEmpty($_.Name) } |
            ForEach-Object { $_.FullName.Replace('\', '/') } |
            Sort-Object -Unique)
    } finally {
        $zip.Dispose()
    }
    $archiveDiff = @(Compare-Object -ReferenceObject $expectedArchiveEntries `
        -DifferenceObject $actualArchiveEntries)
    if ($archiveDiff.Count -gt 0) {
        throw "Runtime ZIP entry mismatch: $($archiveDiff | Out-String)"
    }
    $archiveFile = Get-Item -LiteralPath $archivePath
    $archiveHash = (Get-FileHash -LiteralPath $archivePath -Algorithm SHA256).Hash.ToLowerInvariant()

    $releaseManifest = [pscustomobject][ordered]@{
        schemaVersion = 1
        version = $Version
        platform = 'windows'
        architecture = $Architecture
        flavor = $Flavor
        modelsIncluded = $false
        voiceModelsIncluded = $false
        decoderAssets = $manifest.decoderAssets
        llamaServer = $manifest.llamaServer
        backendExe = $manifest.backendExe
        backendDir = $manifest.backendDir
        backendArgs = $manifest.backendArgs
        executables = $manifest.executables
        payload = $manifest.payload
        archive = [pscustomobject][ordered]@{
            fileName = $archiveFile.Name
            byteSize = [long] $archiveFile.Length
            sha256 = $archiveHash
        }
    }
    $releaseManifestJson = $releaseManifest | ConvertTo-Json -Depth 8
    Write-Utf8NoBom -Path $archiveManifestPath -Content $releaseManifestJson
    # The app's compiled release feed points at this stable alias. Updating it
    # only after the versioned ZIP + sidecar are complete keeps "latest"
    # recoverable and prevents readers from seeing a partially-written file.
    Write-Utf8NoBomAtomic -Path $stableManifestPath -Content $releaseManifestJson

    Write-Host "    Staging: $stagingPath" -ForegroundColor Green
    Write-Host "    Archive: $archivePath" -ForegroundColor Green
    Write-Host "    Feed:    $stableManifestPath" -ForegroundColor Green
    Write-Host "    SHA-256: $archiveHash" -ForegroundColor Green
} finally {
    if (Test-Path -LiteralPath $workRoot) {
        Remove-Item -LiteralPath $workRoot -Recurse -Force -ErrorAction SilentlyContinue
    }
}
