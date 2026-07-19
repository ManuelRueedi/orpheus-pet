#Requires -Version 5.1
<#
.SYNOPSIS
    One-shot dev bootstrap for Orpheus Pet.

    Sets up everything `pnpm tauri dev` needs, so you don't have to run the
    steps in the README by hand:
      1. Python venv + backend deps (PyTorch, SNAC, FastAPI) in Orpheus-FastAPI/venv
      2. llama-server.exe (+ DLLs) in llama/            (best-effort download)
      3. orpheus-pet/stack.config.json                  (copied from the example)
      4. the pet's UI deps                              (pnpm install)

    Re-runnable: anything already present is left alone.

.PARAMETER Cpu
    No NVIDIA GPU: install CPU-only PyTorch and fetch the CPU llama-server build.

.PARAMETER SkipLlama
    Don't download llama-server; you'll drop llama\llama-server.exe in yourself.

.PARAMETER CudaVersion
    PyTorch CUDA wheel channel. By default setup uses cu128 with an R580+
    NVIDIA driver (including RTX 50-series), and cu124 otherwise. Ignored with
    -Cpu.

.EXAMPLE
    .\setup.ps1
    .\setup.ps1 -Cpu
    .\setup.ps1 -CudaVersion cu128
#>
[CmdletBinding()]
param(
    [switch] $Cpu,
    [switch] $SkipLlama,
    [string] $CudaVersion
)

$ErrorActionPreference = 'Stop'
$ProgressPreference     = 'SilentlyContinue'   # faster Invoke-WebRequest downloads
# PS 5.1 doesn't enable TLS 1.2 by default; GitHub / PyPI require it.
[Net.ServicePointManager]::SecurityProtocol =
    [Net.ServicePointManager]::SecurityProtocol -bor [Net.SecurityProtocolType]::Tls12

$Root     = $PSScriptRoot
$Orpheus  = Join-Path $Root 'Orpheus-FastAPI'
$Venv     = Join-Path $Orpheus 'venv'
$VenvPy   = Join-Path $Venv 'Scripts\python.exe'
$Pet      = Join-Path $Root 'orpheus-pet'
$LlamaDir = Join-Path $Root 'llama'

# ---- helpers -------------------------------------------------------------
function Write-Step { param([string]$Msg) Write-Host "`n==> $Msg" -ForegroundColor Cyan }
function Write-Ok   { param([string]$Msg) Write-Host "    OK: $Msg"      -ForegroundColor Green }
function Write-Warn { param([string]$Msg) Write-Host "    WARNING: $Msg" -ForegroundColor Yellow }

# Native exes don't throw on failure; check $LASTEXITCODE right after each call.
function Assert-Native { param([string]$What)
    if ($LASTEXITCODE -ne 0) { throw "$What failed (exit $LASTEXITCODE)" }
}

function Test-Cuda13Driver {
    try {
        $versions = @(& nvidia-smi.exe `
            --query-gpu=driver_version `
            --format=csv,noheader,nounits 2>$null)
        if ($LASTEXITCODE -ne 0) { return $false }
        return @($versions | Where-Object {
            $major = 0
            [int]::TryParse(([string] $_).Trim().Split('.')[0], [ref] $major) -and
                $major -ge 580
        }).Count -gt 0
    } catch {
        return $false
    }
}

# Best-effort: grab the latest llama.cpp Windows build and land llama-server.exe
# (plus its DLLs) in $Dir. Throws on any mismatch so the caller can show manual
# steps -- the exact release asset names drift over time.
function Get-LlamaServer {
    param([string]$Dir, [bool]$CpuBuild)

    $exe = Join-Path $Dir 'llama-server.exe'
    if (Test-Path $exe) { Write-Ok "llama-server already present ($exe)"; return }
    New-Item -ItemType Directory -Force -Path $Dir | Out-Null

    Write-Host '    querying latest llama.cpp release'
    $rel = Invoke-RestMethod -UseBasicParsing `
        -Uri 'https://api.github.com/repos/ggml-org/llama.cpp/releases/latest' `
        -Headers @{ 'User-Agent' = 'orpheus-pet-setup' }
    $assets = $rel.assets

    if ($CpuBuild) {
        $main = $assets | Where-Object { $_.name -match 'bin-win-cpu-x64\.zip$' } | Select-Object -First 1
        if (-not $main) {
            $main = $assets |
                Where-Object { $_.name -match 'bin-win-.*x64\.zip$' -and
                               $_.name -notmatch 'cuda|vulkan|sycl|hip|kompute|musa|arm64' } |
                Select-Object -First 1
        }
        $cudart = $null
    } else {
        # CUDA 13 supports Blackwell but requires an R580+ driver. Keep older
        # NVIDIA systems on the upstream CUDA 12.4 build.
        $cudaAssetVersion = if (Test-Cuda13Driver) { '13(?:\.[0-9]+)*' } else { '12\.4' }
        $main = $assets | Where-Object {
            $_.name -match "bin-win-cuda-$cudaAssetVersion-x64\.zip$"
        } | Select-Object -First 1
        $cudart = $assets | Where-Object {
            $_.name -match "^cudart-.*cuda-$cudaAssetVersion-x64\.zip$"
        } | Select-Object -First 1
    }
    if (-not $main) { throw "no matching llama-server asset in release $($rel.tag_name)" }
    if (-not $CpuBuild -and -not $cudart) {
        throw "no matching CUDA runtime asset in release $($rel.tag_name)"
    }

    $tmp  = Join-Path $env:TEMP ('llama-' + [System.IO.Path]::GetRandomFileName())
    $zips = Join-Path $tmp 'zips'
    $ex   = Join-Path $tmp 'x'
    New-Item -ItemType Directory -Force -Path $zips | Out-Null
    New-Item -ItemType Directory -Force -Path $ex   | Out-Null
    try {
        foreach ($a in @($main, $cudart)) {
            if (-not $a) { continue }
            $zip = Join-Path $zips $a.name
            Write-Host "    downloading $($a.name) ($([math]::Round($a.size / 1MB)) MB)"
            Invoke-WebRequest -UseBasicParsing -Uri $a.browser_download_url -OutFile $zip
            Expand-Archive -Path $zip -DestinationPath $ex -Force
        }
        $found = Get-ChildItem -Path $ex -Recurse -Filter 'llama-server.exe' | Select-Object -First 1
        if (-not $found) { throw 'llama-server.exe not found inside the downloaded archive' }
        # Copy the server's own folder (its sibling DLLs), then sweep in any extra
        # DLLs (e.g. the separate cudart runtime) that landed elsewhere.
        Copy-Item -Path (Join-Path $found.DirectoryName '*') -Destination $Dir -Recurse -Force
        Get-ChildItem -Path $ex -Recurse -Filter '*.dll' |
            Where-Object { -not (Test-Path (Join-Path $Dir $_.Name)) } |
            ForEach-Object { Copy-Item $_.FullName -Destination $Dir -Force }
        if (-not (Test-Path $exe)) { throw 'copy did not land llama-server.exe in llama\' }
        Write-Ok "llama-server installed ($($rel.tag_name))"
    }
    finally {
        Remove-Item -Path $tmp -Recurse -Force -ErrorAction SilentlyContinue
    }
}

# ---- 0. prerequisites ----------------------------------------------------
Write-Step 'Checking prerequisites'
$missing = @()
if (-not (Get-Command node  -ErrorAction SilentlyContinue)) { $missing += 'node  -> https://nodejs.org' }
if (-not (Get-Command pnpm  -ErrorAction SilentlyContinue)) { $missing += 'pnpm  -> npm i -g pnpm' }
if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) { $missing += 'cargo -> https://rustup.rs (+ MSVC "Desktop development with C++")' }

# A Python to bootstrap the venv (prefer python, fall back to the py launcher).
$PyBoot = $null
if     (Get-Command python -ErrorAction SilentlyContinue) { $PyBoot = @('python') }
elseif (Get-Command py     -ErrorAction SilentlyContinue) { $PyBoot = @('py', '-3') }
else   { $missing += 'python 3.10/3.11 -> https://python.org' }

if ($missing.Count -gt 0) {
    Write-Host ''
    Write-Host 'Missing prerequisites:' -ForegroundColor Red
    $missing | ForEach-Object { Write-Host "  - $_" -ForegroundColor Red }
    throw 'Install the above, then re-run setup.ps1.'
}
Write-Ok 'node, pnpm, cargo, python found'

if (-not $Cpu -and [string]::IsNullOrWhiteSpace($CudaVersion)) {
    $CudaVersion = if (Test-Cuda13Driver) { 'cu128' } else { 'cu124' }
}

# ---- 1. Python backend ---------------------------------------------------
Write-Step 'Python backend (Orpheus-FastAPI)'
if (Test-Path $VenvPy) {
    Write-Ok "venv already exists ($Venv)"
} else {
    Write-Host "    creating venv at $Venv"
    if ($PyBoot.Count -eq 1) { & $PyBoot[0] -m venv $Venv }
    else                     { & $PyBoot[0] $PyBoot[1] -m venv $Venv }
    Assert-Native 'python -m venv'
    Write-Ok 'venv created'
}

Write-Host '    upgrading pip'
& $VenvPy -m pip install --upgrade pip --quiet
Assert-Native 'pip upgrade'

# PyTorch first (correct CPU/CUDA build) so SNAC in requirements.txt reuses it
# instead of pulling a default wheel over the network.
if ($Cpu) {
    Write-Host '    installing PyTorch (CPU build)'
    & $VenvPy -m pip install torch --index-url 'https://download.pytorch.org/whl/cpu'
} else {
    Write-Host "    installing PyTorch (CUDA $CudaVersion build)"
    & $VenvPy -m pip install torch --index-url "https://download.pytorch.org/whl/$CudaVersion"
}
Assert-Native 'pip install torch'

Write-Host '    installing backend requirements'
& $VenvPy -m pip install -r (Join-Path $Orpheus 'requirements.txt')
Assert-Native 'pip install -r requirements.txt'
Write-Ok 'backend deps installed'

# ---- 2. llama-server -----------------------------------------------------
Write-Step 'llama-server (GGUF inference)'
if ($SkipLlama) {
    Write-Warn 'skipped (-SkipLlama); put llama-server.exe + its DLLs in llama\ yourself'
} else {
    try {
        Get-LlamaServer -Dir $LlamaDir -CpuBuild:$Cpu.IsPresent
    } catch {
        Write-Warn "automatic llama-server download failed: $($_.Exception.Message)"
        Write-Host '    Do it manually (~2 min):' -ForegroundColor Yellow
        Write-Host '      1. Open  https://github.com/ggml-org/llama.cpp/releases/latest'
        if ($Cpu) {
            Write-Host '      2. Download the  *-bin-win-cpu-x64.zip  asset'
        } else {
            Write-Host '      2. Download the  *-bin-win-cuda-*-x64.zip  asset (+ the matching  cudart-*.zip)'
        }
        Write-Host "      3. Unzip so you have:  $LlamaDir\llama-server.exe  (+ its .dll files)"
    }
}

# ---- 3. config -----------------------------------------------------------
Write-Step 'Config (stack.config.json)'
$cfg = Join-Path $Pet 'stack.config.json'
if (Test-Path $cfg) {
    Write-Ok 'stack.config.json already exists'
} else {
    Copy-Item (Join-Path $Pet 'stack.config.example.json') $cfg
    Write-Ok "created $cfg (tune quant / llamaArgs for your GPU -- see README)"
}

# ---- 4. pet UI -----------------------------------------------------------
Write-Step 'Pet UI (pnpm install)'
Push-Location $Pet
try {
    & pnpm install
    Assert-Native 'pnpm install'
    Write-Ok 'UI deps installed'
} finally {
    Pop-Location
}

# ---- done ----------------------------------------------------------------
Write-Step 'Done'
Write-Host ''
Write-Host '  Next:' -ForegroundColor Green
Write-Host '    cd orpheus-pet'
Write-Host '    pnpm tauri dev'
Write-Host ''
Write-Host '  First run: right-click the witch -> pick a language -> she downloads that voice model.'
Write-Host ''
