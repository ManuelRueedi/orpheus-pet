//! Downloads, verifies, and activates a release runtime pack.
//!
//! The sidecar manifest is the trust anchor: it is fetched only over HTTPS
//! from the compiled release feed (or an explicit process-level override), and
//! binds the archive's exact byte length and SHA-256. The ZIP is still treated
//! as hostile input until every entry and payload hash has been validated.

use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::{
    cmp::Ordering,
    collections::{BTreeMap, HashSet},
    fs::{self, File, OpenOptions},
    io::{self, BufReader, Read, Write},
    path::{Component, Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{
        atomic::{AtomicBool, Ordering as AtomicOrdering},
        Arc, Condvar, Mutex,
    },
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use tauri::{AppHandle, Emitter, Manager};
use url::Url;
use zip::{CompressionMethod, ZipArchive};

#[cfg(windows)]
use std::os::windows::process::CommandExt;

#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

const MANIFEST_SCHEMA: u32 = 1;
const MAX_SIDECAR_BYTES: u64 = 1_048_576;
const MAX_INTERNAL_MANIFEST_BYTES: u64 = 4 * 1_048_576;
const MAX_ARCHIVE_BYTES: u64 = 32 * 1024 * 1024 * 1024;
const MAX_ARCHIVE_PARTS: usize = 256;
const MAX_UNCOMPRESSED_BYTES: u64 = 64 * 1024 * 1024 * 1024;
const MAX_ZIP_ENTRIES: usize = 250_000;
const MAX_PORTABLE_PATH_BYTES: usize = 1024;
const CUDA_13_MIN_DRIVER_MAJOR: u32 = 580;
const HASH_FORMAT: &str =
    "sha256-lowercase, two spaces, portable path, LF; StringComparer.Ordinal path order";

// Release automation publishes stable CPU/CUDA sidecar aliases alongside the
// versioned assets. The sidecar supplies the exact versioned ZIP filename/hash.
const OFFICIAL_FEED_BASE: &str =
    "https://github.com/ManuelRueedi/orpheus-pet/releases/latest/download/";

#[derive(Default)]
struct InstallerActivity {
    active: bool,
    cancelled: bool,
    committing: bool,
    download_child: Option<Arc<Mutex<Child>>>,
}

#[derive(Default)]
pub struct RuntimeInstaller {
    activity: Mutex<InstallerActivity>,
    idle: Condvar,
    exiting: AtomicBool,
}

struct InstallGuard<'a> {
    installer: &'a RuntimeInstaller,
}

impl RuntimeInstaller {
    fn begin(&self) -> Result<InstallGuard<'_>, String> {
        if self.exiting.load(AtomicOrdering::Acquire) {
            return Err("the application is shutting down".to_string());
        }
        let mut activity = self.activity.lock().unwrap();
        if self.exiting.load(AtomicOrdering::Acquire) {
            return Err("the application is shutting down".to_string());
        }
        if activity.active {
            return Err("a runtime download or installation is already in progress".to_string());
        }
        activity.active = true;
        activity.cancelled = false;
        activity.committing = false;
        activity.download_child = None;
        drop(activity);
        Ok(InstallGuard { installer: self })
    }

    fn is_cancelled(&self) -> bool {
        self.exiting.load(AtomicOrdering::Acquire) || self.activity.lock().unwrap().cancelled
    }

    pub fn cancel(&self) -> bool {
        let child = {
            let mut activity = self.activity.lock().unwrap();
            if !activity.active || activity.committing {
                return false;
            }
            activity.cancelled = true;
            activity.download_child.clone()
        };
        if let Some(child) = child {
            let mut child = child.lock().unwrap();
            let _ = child.kill();
            let _ = child.wait();
        }
        true
    }

    fn set_download_child(&self, child: Option<Arc<Mutex<Child>>>) {
        self.activity.lock().unwrap().download_child = child;
    }

    fn begin_commit(&self) -> Result<(), String> {
        let mut activity = self.activity.lock().unwrap();
        if self.exiting.load(AtomicOrdering::Acquire) || activity.cancelled {
            return Err("runtime installation cancelled".to_string());
        }
        activity.committing = true;
        Ok(())
    }

    fn check_cancelled(&self) -> Result<(), String> {
        if self.is_cancelled() {
            Err("runtime installation cancelled".to_string())
        } else {
            Ok(())
        }
    }
}

impl Drop for InstallGuard<'_> {
    fn drop(&mut self) {
        let mut activity = self.installer.activity.lock().unwrap();
        activity.download_child = None;
        activity.committing = false;
        activity.active = false;
        drop(activity);
        self.installer.idle.notify_all();
    }
}

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PayloadSummary {
    byte_size: u64,
    file_count: u64,
    sha256: String,
    hash_format: String,
}

#[derive(Clone, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
struct DecoderFile {
    path: String,
    byte_size: u64,
    sha256: String,
}

#[derive(Clone, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
struct DecoderAssets {
    included: bool,
    repo_id: String,
    revision: String,
    license: String,
    model_root: String,
    license_file: String,
    byte_size: u64,
    files: Vec<DecoderFile>,
}

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PackContract {
    schema_version: u32,
    version: String,
    platform: String,
    architecture: String,
    flavor: String,
    models_included: bool,
    voice_models_included: bool,
    decoder_assets: DecoderAssets,
    llama_server: String,
    backend_exe: String,
    backend_dir: String,
    #[serde(default)]
    backend_args: Vec<String>,
    payload: PayloadSummary,
}

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ArchivePart {
    file_name: String,
    byte_size: u64,
    sha256: String,
}

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ArchiveRecord {
    file_name: String,
    byte_size: u64,
    sha256: String,
    #[serde(default)]
    parts: Vec<ArchivePart>,
}

#[derive(Clone, Deserialize)]
struct SidecarManifest {
    #[serde(flatten)]
    contract: PackContract,
    archive: ArchiveRecord,
}

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct FileRecord {
    path: String,
    byte_size: u64,
    sha256: String,
}

#[derive(Clone, Deserialize)]
struct InternalManifest {
    #[serde(flatten)]
    contract: PackContract,
    files: Vec<FileRecord>,
}

#[derive(Clone)]
struct ValidatedSidecar {
    raw: SidecarManifest,
    archive_url: Url,
    part_urls: Vec<Url>,
    source: &'static str,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeInstallPlan {
    available: bool,
    version: String,
    flavor: String,
    approximate_bytes: u64,
    plan_id: String,
    source: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeInstallResult {
    version: String,
    flavor: String,
    installed_path: String,
    source: String,
}

#[derive(Clone, Copy)]
struct Progress<'a> {
    phase: &'a str,
    pct: u8,
    received: Option<u64>,
    total: Option<u64>,
    message: &'a str,
}

fn emit_progress(app: &AppHandle, progress: Progress<'_>) {
    let _ = app.emit(
        "runtime-progress",
        json!({
            "phase": progress.phase,
            "pct": progress.pct,
            "received": progress.received,
            "total": progress.total,
            "message": progress.message,
        }),
    );
}

fn hidden(command: &mut Command) {
    #[cfg(windows)]
    command.creation_flags(CREATE_NO_WINDOW);
}

fn unique_suffix() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    format!("{}-{nanos}", std::process::id())
}

fn nvidia_driver_supports_cuda_13(output: &str) -> bool {
    output.lines().any(|line| {
        line.trim()
            .split('.')
            .next()
            .and_then(|major| major.parse::<u32>().ok())
            .is_some_and(|major| major >= CUDA_13_MIN_DRIVER_MAJOR)
    })
}

fn detect_nvidia_gpu() -> bool {
    let mut command = Command::new("nvidia-smi.exe");
    command
        .args([
            "--query-gpu=driver_version",
            "--format=csv,noheader,nounits",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    hidden(&mut command);
    let Ok(mut child) = command.spawn() else {
        return false;
    };
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                let mut output = String::new();
                if let Some(mut stdout) = child.stdout.take() {
                    let _ = stdout.read_to_string(&mut output);
                }
                return status.success() && nvidia_driver_supports_cuda_13(&output);
            }
            Ok(None) if Instant::now() < deadline => thread::sleep(Duration::from_millis(50)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return false;
            }
        }
    }
}

fn selected_runtime_flavor() -> Result<String, String> {
    if let Ok(value) = std::env::var("ORPHEUS_PET_RUNTIME_FLAVOR") {
        let value = value.trim().to_ascii_lowercase();
        if matches!(value.as_str(), "cpu" | "cuda") {
            return Ok(value);
        }
        return Err("ORPHEUS_PET_RUNTIME_FLAVOR must be 'cpu' or 'cuda'".to_string());
    }
    Ok(if detect_nvidia_gpu() { "cuda" } else { "cpu" }.to_string())
}

fn resolve_manifest_url() -> Result<(Url, &'static str, Option<String>), String> {
    let explicit = std::env::var("ORPHEUS_PET_RUNTIME_MANIFEST_URL")
        .ok()
        .filter(|value| !value.trim().is_empty());
    if cfg!(debug_assertions) && explicit.is_none() {
        return Err(
            "automatic runtime installation is disabled in development; set ORPHEUS_PET_RUNTIME_MANIFEST_URL explicitly to test it"
                .to_string(),
        );
    }
    let (raw, source, expected_flavor) = if let Some(value) = explicit {
        (value, "environment-override", None)
    } else {
        let flavor = selected_runtime_flavor()?;
        let raw = format!(
            "{OFFICIAL_FEED_BASE}orpheus-runtime-windows-{}-{flavor}.manifest.json",
            expected_architecture()
        );
        (raw, "official-release", Some(flavor))
    };
    let url = Url::parse(raw.trim()).map_err(|e| format!("invalid runtime feed URL: {e}"))?;
    validate_https_url(&url)?;
    Ok((url, source, expected_flavor))
}

fn validate_https_url(url: &Url) -> Result<(), String> {
    if url.scheme() != "https" {
        return Err("runtime feed must use HTTPS".to_string());
    }
    if url.host_str().is_none() || !url.username().is_empty() || url.password().is_some() {
        return Err(
            "runtime feed URL must have a host and must not contain credentials".to_string(),
        );
    }
    if url.fragment().is_some() {
        return Err("runtime feed URL must not contain a fragment".to_string());
    }
    Ok(())
}

fn runtime_root(app: &AppHandle) -> Result<PathBuf, String> {
    app.path()
        .app_local_data_dir()
        .map(|path| path.join("runtime"))
        .map_err(|e| format!("could not resolve runtime directory: {e}"))
}

fn validate_release_token(value: &str, field: &str) -> Result<(), String> {
    let bytes = value.as_bytes();
    if bytes.is_empty()
        || bytes.len() > 64
        || !bytes[0].is_ascii_alphanumeric()
        || bytes
            .iter()
            .any(|byte| !(byte.is_ascii_alphanumeric() || b".+-".contains(byte)))
    {
        return Err(format!(
            "runtime {field} must be a short ASCII release token"
        ));
    }
    if field == "version" && value.eq_ignore_ascii_case("current") {
        return Err("runtime version uses the reserved name 'current'".to_string());
    }
    Ok(())
}

fn expected_architecture() -> &'static str {
    #[cfg(target_arch = "aarch64")]
    {
        "arm64"
    }
    #[cfg(not(target_arch = "aarch64"))]
    {
        "x64"
    }
}

fn validate_sha256(value: &str, field: &str) -> Result<(), String> {
    if value.len() != 64
        || value
            .bytes()
            .any(|byte| !byte.is_ascii_digit() && !(b'a'..=b'f').contains(&byte))
    {
        return Err(format!("{field} must be a lowercase SHA-256 digest"));
    }
    Ok(())
}

fn validate_contract(contract: &PackContract) -> Result<(), String> {
    if contract.schema_version != MANIFEST_SCHEMA {
        return Err(format!(
            "unsupported runtime manifest schema {} (expected {MANIFEST_SCHEMA})",
            contract.schema_version
        ));
    }
    validate_release_token(&contract.version, "version")?;
    validate_release_token(&contract.flavor, "flavor")?;
    if !matches!(contract.flavor.as_str(), "cpu" | "cuda") {
        return Err(format!("unsupported runtime flavor: {}", contract.flavor));
    }
    if contract.platform != "windows" {
        return Err(format!(
            "runtime platform is {}, expected windows",
            contract.platform
        ));
    }
    if contract.architecture != expected_architecture() {
        return Err(format!(
            "runtime architecture is {}, expected {}",
            contract.architecture,
            expected_architecture()
        ));
    }
    if contract.models_included || contract.voice_models_included {
        return Err("runtime packs must not include voice models".to_string());
    }
    let decoder = &contract.decoder_assets;
    if !decoder.included
        || decoder.repo_id.trim().is_empty()
        || decoder.revision.trim().is_empty()
        || decoder.license.trim().is_empty()
        || decoder.model_root != "backend/snac-model"
        || decoder.license_file != "backend/LICENSE-SNAC.txt"
        || decoder.files.is_empty()
        || decoder.byte_size == 0
    {
        return Err("runtime decoder asset contract is missing or invalid".to_string());
    }
    validate_portable_directory_path(&decoder.model_root)?;
    validate_portable_file_path(&decoder.license_file)?;
    let mut decoder_bytes = 0_u64;
    let mut decoder_paths = HashSet::with_capacity(decoder.files.len());
    for file in &decoder.files {
        validate_portable_file_path(&file.path)?;
        validate_sha256(&file.sha256, "decoder file SHA-256")?;
        if file.byte_size == 0 || !decoder_paths.insert(file.path.to_ascii_lowercase()) {
            return Err("runtime decoder file metadata is invalid".to_string());
        }
        decoder_bytes = decoder_bytes
            .checked_add(file.byte_size)
            .ok_or_else(|| "runtime decoder size overflow".to_string())?;
    }
    if decoder_bytes != decoder.byte_size {
        return Err("runtime decoder byte total is invalid".to_string());
    }
    if contract.payload.file_count == 0 || contract.payload.file_count > MAX_ZIP_ENTRIES as u64 {
        return Err("runtime payload file count is invalid".to_string());
    }
    if contract.payload.byte_size == 0 || contract.payload.byte_size > MAX_UNCOMPRESSED_BYTES {
        return Err("runtime payload size is invalid".to_string());
    }
    validate_sha256(&contract.payload.sha256, "payload SHA-256")?;
    if contract.payload.hash_format != HASH_FORMAT {
        return Err("runtime payload hash format is unsupported".to_string());
    }
    validate_portable_file_path(&contract.llama_server)?;
    validate_portable_file_path(&contract.backend_exe)?;
    validate_portable_directory_path(&contract.backend_dir)?;
    if contract.llama_server != "llama/llama-server.exe"
        || contract.backend_exe != "backend/orpheus-backend.exe"
        || contract.backend_dir != "backend"
    {
        return Err("runtime launcher layout does not match the application contract".to_string());
    }
    if !contract.llama_server.ends_with(".exe") || !contract.backend_exe.ends_with(".exe") {
        return Err("runtime launchers must be Windows executables".to_string());
    }
    let backend_prefix = format!("{}/", contract.backend_dir.trim_end_matches('/'));
    if !contract.backend_exe.starts_with(&backend_prefix) {
        return Err("runtime backend executable must be inside backendDir".to_string());
    }
    if contract
        .backend_args
        .iter()
        .any(|arg| arg.contains('\0') || arg.contains('\r') || arg.contains('\n'))
    {
        return Err("runtime backend arguments contain control characters".to_string());
    }
    Ok(())
}

fn validate_sidecar(
    sidecar: SidecarManifest,
    manifest_url: &Url,
    source: &'static str,
) -> Result<ValidatedSidecar, String> {
    validate_contract(&sidecar.contract)?;
    if sidecar.archive.byte_size == 0 || sidecar.archive.byte_size > MAX_ARCHIVE_BYTES {
        return Err("runtime archive size is invalid".to_string());
    }
    validate_sha256(&sidecar.archive.sha256, "archive SHA-256")?;
    validate_portable_file_path(&sidecar.archive.file_name)?;
    if Path::new(&sidecar.archive.file_name).components().count() != 1
        || !sidecar.archive.file_name.ends_with(".zip")
    {
        return Err("runtime archive fileName must be a single .zip filename".to_string());
    }
    let resolve_asset = |file_name: &str, description: &str| -> Result<Url, String> {
        let url = manifest_url
            .join(file_name)
            .map_err(|e| format!("could not resolve runtime {description} URL: {e}"))?;
        validate_https_url(&url)?;
        if url.origin() != manifest_url.origin() {
            return Err(format!(
                "runtime {description} must use the same HTTPS origin as its manifest"
            ));
        }
        Ok(url)
    };
    let archive_url = resolve_asset(&sidecar.archive.file_name, "archive")?;

    if sidecar.archive.parts.len() > MAX_ARCHIVE_PARTS {
        return Err(format!(
            "runtime archive has too many parts (maximum {MAX_ARCHIVE_PARTS})"
        ));
    }
    let mut asset_names = HashSet::with_capacity(sidecar.archive.parts.len() + 1);
    asset_names.insert(sidecar.archive.file_name.to_ascii_lowercase());
    let mut part_bytes = 0_u64;
    let mut part_urls = Vec::with_capacity(sidecar.archive.parts.len());
    for (index, part) in sidecar.archive.parts.iter().enumerate() {
        if part.byte_size == 0 || part.byte_size > MAX_ARCHIVE_BYTES {
            return Err(format!(
                "runtime archive part {} size is invalid",
                index + 1
            ));
        }
        validate_sha256(&part.sha256, "archive part SHA-256")?;
        validate_portable_file_path(&part.file_name)?;
        if Path::new(&part.file_name).components().count() != 1 {
            return Err(format!(
                "runtime archive part {} fileName must be a single filename",
                index + 1
            ));
        }
        if !asset_names.insert(part.file_name.to_ascii_lowercase()) {
            return Err(format!(
                "runtime archive contains a duplicate or case-colliding asset name: {}",
                part.file_name
            ));
        }
        part_bytes = part_bytes
            .checked_add(part.byte_size)
            .ok_or_else(|| "runtime archive part size overflow".to_string())?;
        part_urls.push(resolve_asset(&part.file_name, "archive part")?);
    }
    if !sidecar.archive.parts.is_empty() && part_bytes != sidecar.archive.byte_size {
        return Err(format!(
            "runtime archive parts total {part_bytes} bytes, expected {}",
            sidecar.archive.byte_size
        ));
    }
    Ok(ValidatedSidecar {
        raw: sidecar,
        archive_url,
        part_urls,
        source,
    })
}

fn is_windows_reserved_component(component: &str) -> bool {
    let stem = component
        .split('.')
        .next()
        .unwrap_or(component)
        .to_ascii_uppercase();
    matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL" | "CLOCK$")
        || stem
            .strip_prefix("COM")
            .is_some_and(|n| matches!(n, "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9"))
        || stem
            .strip_prefix("LPT")
            .is_some_and(|n| matches!(n, "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9"))
}

fn validate_portable_path(value: &str, directory: bool) -> Result<Vec<&str>, String> {
    if value.is_empty()
        || value.len() > MAX_PORTABLE_PATH_BYTES
        || !value.is_ascii()
        || value.starts_with('/')
        || value.contains('\\')
    {
        return Err(format!("unsafe runtime path: {value:?}"));
    }
    let trimmed = if directory {
        value.trim_end_matches('/')
    } else {
        if value.ends_with('/') {
            return Err(format!("runtime file path ends with '/': {value:?}"));
        }
        value
    };
    let components: Vec<&str> = trimmed.split('/').collect();
    if components.is_empty()
        || components.iter().any(|component| {
            component.is_empty()
                || *component == "."
                || *component == ".."
                || component.ends_with('.')
                || component.ends_with(' ')
                || component.len() > 255
                || component.bytes().any(|byte| {
                    byte < 0x20 || matches!(byte, b'<' | b'>' | b':' | b'"' | b'|' | b'?' | b'*')
                })
                || is_windows_reserved_component(component)
        })
    {
        return Err(format!("unsafe runtime path: {value:?}"));
    }
    let path = Path::new(trimmed);
    if path.is_absolute()
        || path
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(format!("unsafe runtime path: {value:?}"));
    }
    Ok(components)
}

fn validate_portable_file_path(value: &str) -> Result<(), String> {
    validate_portable_path(value, false).map(|_| ())
}

fn validate_portable_directory_path(value: &str) -> Result<(), String> {
    validate_portable_path(value, true).map(|_| ())
}

fn join_portable(root: &Path, value: &str) -> Result<PathBuf, String> {
    let components = validate_portable_path(value, value.ends_with('/'))?;
    let mut path = root.to_path_buf();
    for component in components {
        path.push(component);
    }
    Ok(path)
}

fn contracts_match(sidecar: &PackContract, internal: &PackContract) -> Result<(), String> {
    validate_contract(internal)?;
    let same = sidecar.schema_version == internal.schema_version
        && sidecar.version == internal.version
        && sidecar.platform == internal.platform
        && sidecar.architecture == internal.architecture
        && sidecar.flavor == internal.flavor
        && sidecar.models_included == internal.models_included
        && sidecar.voice_models_included == internal.voice_models_included
        && sidecar.decoder_assets == internal.decoder_assets
        && sidecar.llama_server == internal.llama_server
        && sidecar.backend_exe == internal.backend_exe
        && sidecar.backend_dir == internal.backend_dir
        && sidecar.backend_args == internal.backend_args
        && sidecar.payload.byte_size == internal.payload.byte_size
        && sidecar.payload.file_count == internal.payload.file_count
        && sidecar.payload.sha256 == internal.payload.sha256
        && sidecar.payload.hash_format == internal.payload.hash_format;
    if !same {
        return Err("internal runtime manifest does not match the trusted sidecar".to_string());
    }
    Ok(())
}

fn read_limited(path: &Path, limit: u64) -> Result<Vec<u8>, String> {
    let metadata =
        fs::metadata(path).map_err(|e| format!("could not inspect {}: {e}", path.display()))?;
    if metadata.len() > limit {
        return Err(format!("{} exceeds the size limit", path.display()));
    }
    fs::read(path).map_err(|e| format!("could not read {}: {e}", path.display()))
}

fn sha256_file(path: &Path, installer: &RuntimeInstaller) -> Result<(u64, String), String> {
    let file = File::open(path).map_err(|e| format!("could not open {}: {e}", path.display()))?;
    let mut reader = BufReader::new(file);
    let mut hasher = Sha256::new();
    let mut buffer = vec![0_u8; 1024 * 1024];
    let mut total = 0_u64;
    loop {
        installer.check_cancelled()?;
        let read = reader
            .read(&mut buffer)
            .map_err(|e| format!("could not hash {}: {e}", path.display()))?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
        total = total
            .checked_add(read as u64)
            .ok_or_else(|| "file size overflow while hashing".to_string())?;
    }
    Ok((total, format!("{:x}", hasher.finalize())))
}

fn parse_sidecar(
    path: &Path,
    manifest_url: &Url,
    source: &'static str,
) -> Result<ValidatedSidecar, String> {
    let bytes = read_limited(path, MAX_SIDECAR_BYTES)?;
    let sidecar: SidecarManifest = serde_json::from_slice(&bytes)
        .map_err(|e| format!("runtime sidecar is invalid JSON: {e}"))?;
    validate_sidecar(sidecar, manifest_url, source)
}

fn curl_download(
    app: &AppHandle,
    installer: &RuntimeInstaller,
    url: &Url,
    destination: &Path,
    maximum_bytes: u64,
    exact_bytes: Option<u64>,
    pct_start: u8,
    pct_span: u8,
    message: &str,
) -> Result<(), String> {
    installer.check_cancelled()?;
    if let Some(parent) = destination.parent() {
        fs::create_dir_all(parent)
            .map_err(|e| format!("could not create {}: {e}", parent.display()))?;
    }
    let error_path = destination.with_extension("curl-error.log");
    let error_log = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&error_path)
        .map_err(|e| format!("could not create download log: {e}"))?;

    let mut command = Command::new("curl.exe");
    command
        .args([
            "--fail",
            "--location",
            "--silent",
            "--show-error",
            "--proto",
            "=https",
            "--proto-redir",
            "=https",
            "--connect-timeout",
            "15",
            "--max-time",
            "7200",
            "--retry",
            "2",
            "--retry-all-errors",
            "--max-filesize",
            &maximum_bytes.to_string(),
            "--output",
        ])
        .arg(destination)
        .arg("--url")
        .arg(url.as_str())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::from(error_log));
    hidden(&mut command);
    let child =
        Arc::new(Mutex::new(command.spawn().map_err(|e| {
            format!("could not start the Windows curl downloader: {e}")
        })?));
    installer.set_download_child(Some(child.clone()));

    let total_for_progress = exact_bytes;
    let started = Instant::now();
    let mut last_size = u64::MAX;
    let mut last_emit = Instant::now() - Duration::from_secs(2);
    let status = loop {
        if installer.is_cancelled() {
            let mut child = child.lock().unwrap();
            let _ = child.kill();
            let _ = child.wait();
            drop(child);
            installer.set_download_child(None);
            let _ = fs::remove_file(destination);
            let _ = fs::remove_file(&error_path);
            return Err("runtime installation cancelled".to_string());
        }
        let size = fs::metadata(destination)
            .map(|meta| meta.len())
            .unwrap_or(0);
        if size > maximum_bytes || exact_bytes.is_some_and(|expected| size > expected) {
            let mut child = child.lock().unwrap();
            let _ = child.kill();
            let _ = child.wait();
            drop(child);
            installer.set_download_child(None);
            return Err("runtime download exceeded its declared size".to_string());
        }
        if size != last_size || last_emit.elapsed() >= Duration::from_secs(1) {
            let fraction = total_for_progress
                .filter(|total| *total > 0)
                .map(|total| (size as f64 / total as f64).clamp(0.0, 1.0))
                .unwrap_or(0.0);
            let pct = pct_start.saturating_add((fraction * f64::from(pct_span)).round() as u8);
            emit_progress(
                app,
                Progress {
                    phase: "downloading",
                    pct,
                    received: Some(size),
                    total: total_for_progress,
                    message,
                },
            );
            last_size = size;
            last_emit = Instant::now();
        }
        let wait_result = {
            let mut child = child.lock().unwrap();
            child.try_wait()
        };
        let status = match wait_result {
            Ok(status) => status,
            Err(error) => {
                let mut child = child.lock().unwrap();
                let _ = child.kill();
                let _ = child.wait();
                drop(child);
                installer.set_download_child(None);
                let _ = fs::remove_file(destination);
                let _ = fs::remove_file(&error_path);
                return Err(format!("could not monitor runtime download: {error}"));
            }
        };
        if let Some(status) = status {
            break status;
        }
        if started.elapsed() > Duration::from_secs(7205) {
            let mut child = child.lock().unwrap();
            let _ = child.kill();
            let _ = child.wait();
            drop(child);
            installer.set_download_child(None);
            return Err("runtime download timed out".to_string());
        }
        thread::sleep(Duration::from_millis(250));
    };
    installer.set_download_child(None);

    if !status.success() {
        let detail = read_limited(&error_path, 16 * 1024)
            .ok()
            .and_then(|bytes| String::from_utf8(bytes).ok())
            .map(|text| text.trim().chars().take(1000).collect::<String>())
            .filter(|text| !text.is_empty())
            .unwrap_or_else(|| format!("curl exited with {status}"));
        let _ = fs::remove_file(&error_path);
        return Err(format!("runtime download failed: {detail}"));
    }
    let _ = fs::remove_file(&error_path);
    let size = fs::metadata(destination)
        .map_err(|e| format!("runtime download disappeared: {e}"))?
        .len();
    if exact_bytes.is_some_and(|expected| size != expected) {
        return Err(format!(
            "runtime download size mismatch: expected {} bytes, received {size}",
            exact_bytes.unwrap_or_default()
        ));
    }
    if size > maximum_bytes {
        return Err("runtime download exceeded its size limit".to_string());
    }
    Ok(())
}

fn append_file(
    source: &Path,
    destination: &mut File,
    installer: &RuntimeInstaller,
) -> Result<u64, String> {
    let file = File::open(source).map_err(|e| {
        format!(
            "could not open runtime archive part {}: {e}",
            source.display()
        )
    })?;
    let mut reader = BufReader::new(file);
    let mut buffer = vec![0_u8; 1024 * 1024];
    let mut total = 0_u64;
    loop {
        installer.check_cancelled()?;
        let read = reader.read(&mut buffer).map_err(|e| {
            format!(
                "could not read runtime archive part {}: {e}",
                source.display()
            )
        })?;
        if read == 0 {
            break;
        }
        destination
            .write_all(&buffer[..read])
            .map_err(|e| format!("could not assemble the runtime archive: {e}"))?;
        total = total
            .checked_add(read as u64)
            .ok_or_else(|| "runtime archive assembly size overflow".to_string())?;
    }
    Ok(total)
}

fn archive_progress_pct(received: u64, total: u64) -> u8 {
    let fraction = if total == 0 {
        0.0
    } else {
        (received as f64 / total as f64).clamp(0.0, 1.0)
    };
    5_u8.saturating_add((fraction * 53.0).round() as u8)
}

fn download_runtime_archive(
    app: &AppHandle,
    installer: &RuntimeInstaller,
    sidecar: &ValidatedSidecar,
    work: &Path,
    archive_path: &Path,
) -> Result<(), String> {
    if sidecar.raw.archive.parts.is_empty() {
        return curl_download(
            app,
            installer,
            &sidecar.archive_url,
            archive_path,
            sidecar.raw.archive.byte_size,
            Some(sidecar.raw.archive.byte_size),
            5,
            53,
            "Downloading the speech runtime",
        );
    }

    let mut archive = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(archive_path)
        .map_err(|e| format!("could not create the assembled runtime archive: {e}"))?;
    let part_count = sidecar.raw.archive.parts.len();
    let mut assembled_bytes = 0_u64;

    for (index, (part, url)) in sidecar
        .raw
        .archive
        .parts
        .iter()
        .zip(&sidecar.part_urls)
        .enumerate()
    {
        installer.check_cancelled()?;
        let part_path = work.join(format!("runtime-archive-part-{:03}.download", index + 1));
        let start_pct = archive_progress_pct(assembled_bytes, sidecar.raw.archive.byte_size);
        let end_bytes = assembled_bytes
            .checked_add(part.byte_size)
            .ok_or_else(|| "runtime archive part size overflow".to_string())?;
        let end_pct = archive_progress_pct(end_bytes, sidecar.raw.archive.byte_size);
        let message = format!(
            "Downloading speech runtime part {}/{}",
            index + 1,
            part_count
        );
        curl_download(
            app,
            installer,
            url,
            &part_path,
            part.byte_size,
            Some(part.byte_size),
            start_pct,
            end_pct.saturating_sub(start_pct),
            &message,
        )?;

        emit_progress(
            app,
            Progress {
                phase: "verifying-part",
                pct: end_pct,
                received: Some(end_bytes),
                total: Some(sidecar.raw.archive.byte_size),
                message: "Verifying a runtime download part",
            },
        );
        let (part_size, part_hash) = sha256_file(&part_path, installer)?;
        if part_size != part.byte_size || part_hash != part.sha256 {
            return Err(format!(
                "runtime archive part {} failed size or SHA-256 verification",
                index + 1
            ));
        }
        let appended = append_file(&part_path, &mut archive, installer)?;
        if appended != part.byte_size {
            return Err(format!(
                "runtime archive part {} changed while it was assembled",
                index + 1
            ));
        }
        assembled_bytes = end_bytes;
        let _ = fs::remove_file(&part_path);
    }
    archive
        .flush()
        .map_err(|e| format!("could not flush the assembled runtime archive: {e}"))?;
    drop(archive);
    if assembled_bytes != sidecar.raw.archive.byte_size {
        return Err(format!(
            "assembled runtime archive size mismatch: expected {}, wrote {assembled_bytes}",
            sidecar.raw.archive.byte_size
        ));
    }
    Ok(())
}

fn fetch_sidecar(
    app: &AppHandle,
    installer: &RuntimeInstaller,
    work: &Path,
) -> Result<ValidatedSidecar, String> {
    let (url, source, expected_flavor) = resolve_manifest_url()?;
    emit_progress(
        app,
        Progress {
            phase: "checking",
            pct: 1,
            received: None,
            total: None,
            message: "Checking the trusted runtime release",
        },
    );
    let path = work.join("runtime-sidecar.json.part");
    curl_download(
        app,
        installer,
        &url,
        &path,
        MAX_SIDECAR_BYTES,
        None,
        1,
        3,
        "Downloading runtime metadata",
    )?;
    installer.check_cancelled()?;
    let sidecar = parse_sidecar(&path, &url, source)?;
    if expected_flavor
        .as_deref()
        .is_some_and(|expected| sidecar.raw.contract.flavor != expected)
    {
        return Err("runtime feed returned the wrong CPU/CUDA flavor".to_string());
    }
    Ok(sidecar)
}

fn zip_mode_is_safe(mode: Option<u32>, is_directory: bool) -> bool {
    let Some(mode) = mode else {
        return true;
    };
    let kind = mode & 0o170000;
    if is_directory {
        kind == 0 || kind == 0o040000
    } else {
        kind == 0 || kind == 0o100000
    }
}

fn expected_zip_prefix(contract: &PackContract) -> String {
    format!("runtime/{}/{}/", contract.version, contract.flavor)
}

fn safe_extract_zip(
    archive_path: &Path,
    extraction_root: &Path,
    sidecar: &ValidatedSidecar,
    installer: &RuntimeInstaller,
    app: &AppHandle,
) -> Result<PathBuf, String> {
    let file =
        File::open(archive_path).map_err(|e| format!("could not open runtime archive: {e}"))?;
    let mut archive =
        ZipArchive::new(file).map_err(|e| format!("runtime archive is not a valid ZIP: {e}"))?;
    let entry_count = archive.len();
    if entry_count == 0 || entry_count > MAX_ZIP_ENTRIES {
        return Err("runtime archive has an invalid number of entries".to_string());
    }
    fs::create_dir(extraction_root)
        .map_err(|e| format!("could not create extraction directory: {e}"))?;

    let prefix = expected_zip_prefix(&sidecar.raw.contract);
    let mut names = HashSet::with_capacity(entry_count);
    let mut declared_uncompressed = 0_u64;
    let declared_limit = sidecar
        .raw
        .contract
        .payload
        .byte_size
        .checked_add(MAX_INTERNAL_MANIFEST_BYTES)
        .ok_or_else(|| "runtime payload size overflow".to_string())?;

    for index in 0..entry_count {
        installer.check_cancelled()?;
        let mut entry = archive
            .by_index(index)
            .map_err(|e| format!("could not read ZIP entry {index}: {e}"))?;
        let raw_name = entry.name_raw().to_vec();
        let name = std::str::from_utf8(&raw_name)
            .map_err(|_| "runtime ZIP contains a non-UTF-8 path".to_string())?
            .to_string();
        let is_directory = entry.is_dir();
        if entry.encrypted() {
            return Err(format!("runtime ZIP entry is encrypted: {name:?}"));
        }
        if !matches!(
            entry.compression(),
            CompressionMethod::Stored | CompressionMethod::Deflated
        ) {
            return Err(format!(
                "runtime ZIP uses an unsupported compression method: {name:?}"
            ));
        }
        validate_portable_path(&name, is_directory)?;
        if !name.starts_with(&prefix) || name.len() <= prefix.len() {
            return Err(format!(
                "runtime ZIP entry is outside {}: {name:?}",
                prefix.trim_end_matches('/')
            ));
        }
        if !zip_mode_is_safe(entry.unix_mode(), is_directory) {
            return Err(format!(
                "runtime ZIP contains a link or special file: {name:?}"
            ));
        }
        let collision_key = name.to_ascii_lowercase();
        if !names.insert(collision_key) {
            return Err(format!(
                "runtime ZIP contains a duplicate or case-colliding path: {name:?}"
            ));
        }
        declared_uncompressed = declared_uncompressed
            .checked_add(entry.size())
            .ok_or_else(|| "runtime ZIP size overflow".to_string())?;
        if declared_uncompressed > declared_limit || declared_uncompressed > MAX_UNCOMPRESSED_BYTES
        {
            return Err("runtime ZIP expands beyond its declared payload size".to_string());
        }

        let destination = join_portable(extraction_root, &name)?;
        if is_directory {
            if entry.size() != 0 {
                return Err(format!("runtime ZIP directory has data: {name:?}"));
            }
            fs::create_dir_all(&destination)
                .map_err(|e| format!("could not create {}: {e}", destination.display()))?;
        } else {
            if let Some(parent) = destination.parent() {
                fs::create_dir_all(parent)
                    .map_err(|e| format!("could not create {}: {e}", parent.display()))?;
            }
            let mut output = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&destination)
                .map_err(|e| format!("could not create {}: {e}", destination.display()))?;
            let expected = entry.size();
            let copied = io::copy(
                &mut entry.by_ref().take(expected.saturating_add(1)),
                &mut output,
            )
            .map_err(|e| format!("could not extract {name:?}: {e}"))?;
            output
                .flush()
                .map_err(|e| format!("could not flush {name:?}: {e}"))?;
            if copied != expected {
                return Err(format!(
                    "runtime ZIP entry size mismatch for {name:?}: expected {expected}, wrote {copied}"
                ));
            }
        }

        if index % 20 == 0 || index + 1 == entry_count {
            let pct = 60_u8
                .saturating_add(((index + 1) as f64 / entry_count as f64 * 18.0).round() as u8);
            emit_progress(
                app,
                Progress {
                    phase: "extracting",
                    pct,
                    received: Some((index + 1) as u64),
                    total: Some(entry_count as u64),
                    message: "Unpacking the speech runtime safely",
                },
            );
        }
    }

    Ok(extraction_root
        .join("runtime")
        .join(&sidecar.raw.contract.version)
        .join(&sidecar.raw.contract.flavor))
}

fn portable_relative(root: &Path, path: &Path) -> Result<String, String> {
    let relative = path
        .strip_prefix(root)
        .map_err(|_| format!("{} is outside {}", path.display(), root.display()))?;
    let mut parts = Vec::new();
    for component in relative.components() {
        let Component::Normal(value) = component else {
            return Err(format!("unsafe extracted path: {}", path.display()));
        };
        let value = value
            .to_str()
            .ok_or_else(|| format!("non-UTF-8 extracted path: {}", path.display()))?;
        parts.push(value);
    }
    let portable = parts.join("/");
    validate_portable_file_path(&portable)?;
    Ok(portable)
}

fn collect_regular_files(root: &Path) -> Result<BTreeMap<String, PathBuf>, String> {
    fn visit(
        root: &Path,
        directory: &Path,
        files: &mut BTreeMap<String, PathBuf>,
        collision_keys: &mut HashSet<String>,
    ) -> Result<(), String> {
        for entry in fs::read_dir(directory)
            .map_err(|e| format!("could not inspect {}: {e}", directory.display()))?
        {
            let entry = entry.map_err(|e| format!("could not inspect runtime entry: {e}"))?;
            let path = entry.path();
            let metadata = fs::symlink_metadata(&path)
                .map_err(|e| format!("could not inspect {}: {e}", path.display()))?;
            if metadata.file_type().is_symlink() {
                return Err(format!(
                    "runtime payload contains a link/reparse point: {}",
                    path.display()
                ));
            }
            if metadata.is_dir() {
                visit(root, &path, files, collision_keys)?;
            } else if metadata.is_file() {
                let portable = portable_relative(root, &path)?;
                if !collision_keys.insert(portable.to_ascii_lowercase()) {
                    return Err(format!(
                        "runtime payload has case-colliding paths: {portable}"
                    ));
                }
                files.insert(portable, path);
            } else {
                return Err(format!(
                    "runtime payload contains a special file: {}",
                    path.display()
                ));
            }
        }
        Ok(())
    }

    let metadata =
        fs::symlink_metadata(root).map_err(|e| format!("runtime payload root is missing: {e}"))?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err("runtime payload root must be a real directory".to_string());
    }
    let mut files = BTreeMap::new();
    let mut collision_keys = HashSet::new();
    visit(root, root, &mut files, &mut collision_keys)?;
    Ok(files)
}

fn ordinal_cmp(left: &str, right: &str) -> Ordering {
    left.encode_utf16().cmp(right.encode_utf16())
}

fn forbidden_payload_path(path: &str) -> bool {
    let lower = path.to_ascii_lowercase();
    if lower == "backend/snac-model/pytorch_model.bin" {
        return false;
    }
    let name = lower.rsplit('/').next().unwrap_or(&lower);
    name == ".env"
        || name == "stack.config.json"
        || name == "pyvenv.cfg"
        || [".gguf", ".safetensors", ".pt", ".pth", ".ckpt", ".bin"]
            .iter()
            .any(|suffix| lower.ends_with(suffix))
}

fn verify_file_record(
    path: &Path,
    record: &FileRecord,
    installer: &RuntimeInstaller,
) -> Result<(), String> {
    validate_sha256(&record.sha256, "payload file SHA-256")?;
    let (size, hash) = sha256_file(path, installer)?;
    if size != record.byte_size {
        return Err(format!(
            "payload size mismatch for {}: expected {}, found {size}",
            record.path, record.byte_size
        ));
    }
    if hash != record.sha256 {
        return Err(format!("payload SHA-256 mismatch for {}", record.path));
    }
    Ok(())
}

fn verify_pack(
    pack_root: &Path,
    sidecar: &ValidatedSidecar,
    installer: &RuntimeInstaller,
    app: &AppHandle,
) -> Result<InternalManifest, String> {
    installer.check_cancelled()?;
    let manifest_path = pack_root.join("manifest.json");
    let bytes = read_limited(&manifest_path, MAX_INTERNAL_MANIFEST_BYTES)?;
    let internal: InternalManifest = serde_json::from_slice(&bytes)
        .map_err(|e| format!("internal runtime manifest is invalid JSON: {e}"))?;
    contracts_match(&sidecar.raw.contract, &internal.contract)?;
    if internal.files.len() as u64 != internal.contract.payload.file_count {
        return Err("internal runtime file count does not match its payload summary".to_string());
    }

    let mut records: Vec<&FileRecord> = internal.files.iter().collect();
    records.sort_by(|left, right| ordinal_cmp(&left.path, &right.path));
    let mut record_keys = HashSet::with_capacity(records.len());
    for record in &records {
        validate_portable_file_path(&record.path)?;
        if record.path.eq_ignore_ascii_case("manifest.json") {
            return Err("manifest.json must not hash itself".to_string());
        }
        if forbidden_payload_path(&record.path) {
            return Err(format!(
                "runtime payload contains forbidden model or machine state: {}",
                record.path
            ));
        }
        if !record_keys.insert(record.path.to_ascii_lowercase()) {
            return Err(format!(
                "runtime manifest contains duplicate or case-colliding paths: {}",
                record.path
            ));
        }
    }

    let mut actual = collect_regular_files(pack_root)?;
    actual
        .remove("manifest.json")
        .ok_or_else(|| "runtime payload is missing manifest.json".to_string())?;
    if actual.len() != records.len() {
        return Err(format!(
            "runtime payload file set mismatch: manifest lists {}, archive contains {}",
            records.len(),
            actual.len()
        ));
    }

    let mut payload_bytes = 0_u64;
    let mut tree_hasher = Sha256::new();
    for (index, record) in records.iter().enumerate() {
        installer.check_cancelled()?;
        let path = actual
            .get(&record.path)
            .ok_or_else(|| format!("runtime payload is missing {}", record.path))?;
        verify_file_record(path, record, installer)?;
        payload_bytes = payload_bytes
            .checked_add(record.byte_size)
            .ok_or_else(|| "runtime payload size overflow".to_string())?;
        tree_hasher.update(record.sha256.as_bytes());
        tree_hasher.update(b"  ");
        tree_hasher.update(record.path.as_bytes());
        tree_hasher.update(b"\n");

        if index % 10 == 0 || index + 1 == records.len() {
            let pct = 78_u8
                .saturating_add(((index + 1) as f64 / records.len() as f64 * 10.0).round() as u8);
            emit_progress(
                app,
                Progress {
                    phase: "verifying",
                    pct,
                    received: Some((index + 1) as u64),
                    total: Some(records.len() as u64),
                    message: "Verifying every runtime file",
                },
            );
        }
    }
    if payload_bytes != internal.contract.payload.byte_size {
        return Err(format!(
            "runtime payload byte total mismatch: expected {}, found {payload_bytes}",
            internal.contract.payload.byte_size
        ));
    }
    let tree_hash = format!("{:x}", tree_hasher.finalize());
    if tree_hash != internal.contract.payload.sha256 {
        return Err("runtime payload tree SHA-256 mismatch".to_string());
    }

    for launcher in [
        &internal.contract.llama_server,
        &internal.contract.backend_exe,
    ] {
        if !actual.contains_key(launcher) {
            return Err(format!(
                "runtime launcher is missing from payload: {launcher}"
            ));
        }
    }
    let decoder = &internal.contract.decoder_assets;
    for required in [
        decoder.license_file.as_str(),
        "backend/snac-model/orpheus-snac.json",
    ] {
        if !actual.contains_key(required) {
            return Err(format!("runtime decoder asset is missing: {required}"));
        }
    }
    for decoder_file in &decoder.files {
        let payload_path = format!("{}/{}", decoder.model_root, decoder_file.path);
        let record = records
            .iter()
            .find(|record| record.path == payload_path)
            .ok_or_else(|| format!("runtime decoder asset is missing: {payload_path}"))?;
        if record.byte_size != decoder_file.byte_size || record.sha256 != decoder_file.sha256 {
            return Err(format!(
                "runtime decoder metadata does not match payload hash: {payload_path}"
            ));
        }
    }
    let backend_dir = join_portable(pack_root, &internal.contract.backend_dir)?;
    let metadata = fs::symlink_metadata(&backend_dir)
        .map_err(|e| format!("runtime backendDir is missing: {e}"))?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err("runtime backendDir must be a real directory".to_string());
    }
    Ok(internal)
}

fn ensure_real_directory(path: &Path) -> Result<(), String> {
    if path.exists() {
        let metadata = fs::symlink_metadata(path)
            .map_err(|e| format!("could not inspect {}: {e}", path.display()))?;
        if !metadata.is_dir() || metadata.file_type().is_symlink() {
            return Err(format!(
                "{} must be a real directory, not a file or link",
                path.display()
            ));
        }
    } else {
        fs::create_dir_all(path)
            .map_err(|e| format!("could not create {}: {e}", path.display()))?;
        let metadata = fs::symlink_metadata(path)
            .map_err(|e| format!("could not inspect {}: {e}", path.display()))?;
        if !metadata.is_dir() || metadata.file_type().is_symlink() {
            return Err(format!("{} is not a real directory", path.display()));
        }
    }
    Ok(())
}

fn remove_owned_path(path: &Path) {
    let Ok(metadata) = fs::symlink_metadata(path) else {
        return;
    };
    if metadata.is_dir() && !metadata.file_type().is_symlink() {
        let _ = fs::remove_dir_all(path);
    } else {
        let _ = fs::remove_file(path).or_else(|_| fs::remove_dir(path));
    }
}

fn recover_interrupted_activation(runtime_root: &Path) -> Result<(), String> {
    let current = runtime_root.join("current");
    let mut previous = Vec::new();
    for entry in fs::read_dir(runtime_root)
        .map_err(|e| format!("could not inspect runtime activation state: {e}"))?
    {
        let entry = entry.map_err(|e| format!("could not inspect activation entry: {e}"))?;
        let name = entry.file_name();
        if name
            .to_str()
            .is_some_and(|name| name.starts_with(".current-previous-"))
        {
            let metadata = fs::symlink_metadata(entry.path())
                .map_err(|e| format!("could not inspect interrupted activation: {e}"))?;
            if metadata.is_dir() && !metadata.file_type().is_symlink() {
                previous.push(entry.path());
            }
        }
    }
    previous.sort();
    if let Some(candidate) = previous.pop() {
        if current.exists() {
            let metadata = fs::symlink_metadata(&current)
                .map_err(|e| format!("could not inspect interrupted runtime: {e}"))?;
            if !metadata.is_dir() || metadata.file_type().is_symlink() {
                return Err("active runtime must be a real directory during recovery".to_string());
            }
            let interrupted =
                runtime_root.join(format!(".current-interrupted-{}", unique_suffix()));
            fs::rename(&current, &interrupted)
                .map_err(|e| format!("could not move interrupted runtime aside: {e}"))?;
            if let Err(error) = fs::rename(&candidate, &current) {
                let undo = fs::rename(&interrupted, &current);
                return Err(format!(
                    "could not recover the previous runtime {}: {error}; interrupted-runtime recovery: {}",
                    candidate.display(),
                    undo.err()
                        .map(|e| e.to_string())
                        .unwrap_or_else(|| "restored".to_string())
                ));
            }
            remove_owned_path(&interrupted);
        } else {
            fs::rename(&candidate, &current).map_err(|e| {
                format!(
                    "could not recover the previous runtime {}: {e}",
                    candidate.display()
                )
            })?;
        }
    }
    Ok(())
}

pub fn recover_on_start(app: &AppHandle) -> Result<(), String> {
    let root = runtime_root(app)?;
    if !root.exists() {
        return Ok(());
    }
    ensure_real_directory(&root)?;
    recover_interrupted_activation(&root)
}

fn clone_tree(
    source: &Path,
    destination: &Path,
    installer: &RuntimeInstaller,
) -> Result<(), String> {
    fn clone_directory(
        source: &Path,
        destination: &Path,
        installer: &RuntimeInstaller,
    ) -> Result<(), String> {
        installer.check_cancelled()?;
        fs::create_dir(destination)
            .map_err(|e| format!("could not create {}: {e}", destination.display()))?;
        for entry in fs::read_dir(source)
            .map_err(|e| format!("could not inspect {}: {e}", source.display()))?
        {
            installer.check_cancelled()?;
            let entry = entry.map_err(|e| format!("could not inspect runtime file: {e}"))?;
            let source_path = entry.path();
            let destination_path = destination.join(entry.file_name());
            let metadata = fs::symlink_metadata(&source_path)
                .map_err(|e| format!("could not inspect {}: {e}", source_path.display()))?;
            if metadata.file_type().is_symlink() {
                return Err(format!(
                    "runtime source contains a link/reparse point: {}",
                    source_path.display()
                ));
            }
            if metadata.is_dir() {
                clone_directory(&source_path, &destination_path, installer)?;
            } else if metadata.is_file() {
                if fs::hard_link(&source_path, &destination_path).is_err() {
                    let copied = fs::copy(&source_path, &destination_path).map_err(|e| {
                        format!(
                            "could not copy {} to {}: {e}",
                            source_path.display(),
                            destination_path.display()
                        )
                    })?;
                    if copied != metadata.len() {
                        return Err(format!(
                            "short copy while activating {}",
                            source_path.display()
                        ));
                    }
                }
            } else {
                return Err(format!(
                    "runtime source contains a special file: {}",
                    source_path.display()
                ));
            }
        }
        Ok(())
    }

    if destination.exists() {
        return Err(format!(
            "activation staging path already exists: {}",
            destination.display()
        ));
    }
    clone_directory(source, destination, installer)
}

fn install_versioned_pack(
    extracted_pack: &Path,
    runtime_root: &Path,
    sidecar: &ValidatedSidecar,
    installer: &RuntimeInstaller,
    app: &AppHandle,
    suffix: &str,
) -> Result<PathBuf, String> {
    let version_parent = runtime_root.join(&sidecar.raw.contract.version);
    ensure_real_directory(&version_parent)?;
    let target = version_parent.join(&sidecar.raw.contract.flavor);
    if target.exists() {
        match verify_pack(&target, sidecar, installer, app) {
            Ok(_) => return Ok(target),
            Err(error) if installer.is_cancelled() => return Err(error),
            Err(error) => {
                let quarantine = runtime_root.join(format!(
                    ".invalid-{}-{}-{suffix}",
                    sidecar.raw.contract.version, sidecar.raw.contract.flavor
                ));
                fs::rename(&target, &quarantine).map_err(|rename_error| {
                    format!(
                        "installed runtime is corrupt ({error}) and could not be quarantined: {rename_error}"
                    )
                })?;
                if let Err(rename_error) = fs::rename(extracted_pack, &target) {
                    let rollback = fs::rename(&quarantine, &target);
                    return Err(format!(
                        "could not replace corrupt runtime: {rename_error}; rollback: {}",
                        rollback
                            .err()
                            .map(|e| e.to_string())
                            .unwrap_or_else(|| "restored".to_string())
                    ));
                }
                return Ok(target);
            }
        }
    }
    fs::rename(extracted_pack, &target)
        .map_err(|e| format!("could not install versioned runtime: {e}"))?;
    Ok(target)
}

struct PendingActivation {
    current: PathBuf,
    previous: Option<PathBuf>,
}

impl PendingActivation {
    fn has_previous(&self) -> bool {
        self.previous.is_some()
    }

    fn commit(&mut self, suffix: &str) -> Result<(), String> {
        if let Some(previous) = self.previous.as_ref() {
            // Renaming the rollback marker is the durable commit. Cleanup may
            // be interrupted without making the next launch roll back a pack
            // that already passed endpoint readiness.
            let retired = previous
                .parent()
                .ok_or_else(|| "rollback runtime has no parent directory".to_string())?
                .join(format!(".current-retired-{suffix}"));
            fs::rename(previous, &retired)
                .map_err(|e| format!("could not seal runtime activation: {e}"))?;
            self.previous = None;
            remove_owned_path(&retired);
        }
        Ok(())
    }

    fn rollback(self, suffix: &str) -> Result<(), String> {
        let Some(previous) = self.previous else {
            // A first installation has nothing known-good to restore. Keep the
            // verified runtime active so a later retry does not redownload it.
            return Ok(());
        };
        let runtime_root = self
            .current
            .parent()
            .ok_or_else(|| "active runtime has no parent directory".to_string())?;
        let failed = runtime_root.join(format!(".current-failed-{suffix}"));
        fs::rename(&self.current, &failed)
            .map_err(|e| format!("could not move the failed runtime aside: {e}"))?;
        if let Err(error) = fs::rename(&previous, &self.current) {
            let undo = fs::rename(&failed, &self.current);
            return Err(format!(
                "could not restore the previous runtime: {error}; failed-runtime recovery: {}",
                undo.err()
                    .map(|e| e.to_string())
                    .unwrap_or_else(|| "restored".to_string())
            ));
        }
        remove_owned_path(&failed);
        Ok(())
    }
}

fn activate_candidate(
    runtime_root: &Path,
    candidate: &Path,
    suffix: &str,
) -> Result<PendingActivation, String> {
    let current = runtime_root.join("current");
    let previous = runtime_root.join(format!(".current-previous-{suffix}"));
    let had_current = current.exists();
    if had_current {
        fs::rename(&current, &previous)
            .map_err(|e| format!("could not move the active runtime aside: {e}"))?;
    }
    if let Err(error) = fs::rename(candidate, &current) {
        let rollback = if had_current {
            fs::rename(&previous, &current)
        } else {
            Ok(())
        };
        return Err(format!(
            "could not activate the new runtime: {error}; rollback: {}",
            rollback
                .err()
                .map(|e| e.to_string())
                .unwrap_or_else(|| "restored".to_string())
        ));
    }
    Ok(PendingActivation {
        current,
        previous: had_current.then_some(previous),
    })
}

struct WorkDirectory(PathBuf);

impl Drop for WorkDirectory {
    fn drop(&mut self) {
        remove_owned_path(&self.0);
    }
}

fn prepare_work_directory(runtime_root: &Path, suffix: &str) -> Result<WorkDirectory, String> {
    ensure_real_directory(runtime_root)?;
    recover_interrupted_activation(runtime_root)?;
    let path = runtime_root.join(format!(".install-work-{suffix}"));
    fs::create_dir(&path)
        .map_err(|e| format!("could not create runtime installation workspace: {e}"))?;
    Ok(WorkDirectory(path))
}

fn runtime_plan_blocking(
    app: &AppHandle,
    installer: &RuntimeInstaller,
) -> Result<RuntimeInstallPlan, String> {
    let _session = installer.begin()?;
    let root = runtime_root(app)?;
    let suffix = unique_suffix();
    let work = prepare_work_directory(&root, &suffix)?;
    let sidecar = fetch_sidecar(app, installer, &work.0)?;
    emit_progress(
        app,
        Progress {
            phase: "plan-ready",
            pct: 100,
            received: Some(sidecar.raw.archive.byte_size),
            total: Some(sidecar.raw.archive.byte_size),
            message: "Runtime release is available",
        },
    );
    Ok(RuntimeInstallPlan {
        available: true,
        version: sidecar.raw.contract.version.clone(),
        flavor: sidecar.raw.contract.flavor.clone(),
        approximate_bytes: sidecar.raw.archive.byte_size,
        plan_id: runtime_plan_id(&sidecar),
        source: sidecar.source.to_string(),
    })
}

// Consent is tied to the exact release contract shown by runtime_install_plan.
// The stable GitHub alias can advance between the plan request and the click;
// refuse that drift instead of silently downloading a different pack or size.
fn runtime_plan_id(sidecar: &ValidatedSidecar) -> String {
    format!(
        "{}:{}:{}:{}",
        sidecar.raw.contract.version,
        sidecar.raw.contract.flavor,
        sidecar.raw.archive.byte_size,
        sidecar.raw.archive.sha256
    )
}

fn install_runtime_blocking(
    app: &AppHandle,
    installer: &RuntimeInstaller,
    expected_plan_id: &str,
) -> Result<RuntimeInstallResult, String> {
    let _session = installer.begin()?;
    let root = runtime_root(app)?;
    let suffix = unique_suffix();
    let work = prepare_work_directory(&root, &suffix)?;
    let sidecar = fetch_sidecar(app, installer, &work.0)?;
    if expected_plan_id != runtime_plan_id(&sidecar) {
        return Err(
            "the runtime release changed after it was shown; review the updated download and try again"
                .to_string(),
        );
    }
    installer.check_cancelled()?;

    let archive_path = work.0.join("runtime.zip.part");
    download_runtime_archive(app, installer, &sidecar, &work.0, &archive_path)?;
    emit_progress(
        app,
        Progress {
            phase: "verifying-archive",
            pct: 59,
            received: Some(sidecar.raw.archive.byte_size),
            total: Some(sidecar.raw.archive.byte_size),
            message: "Verifying the downloaded archive",
        },
    );
    let (_, archive_hash) = sha256_file(&archive_path, installer)?;
    if archive_hash != sidecar.raw.archive.sha256 {
        return Err("runtime archive SHA-256 mismatch".to_string());
    }

    let extraction_root = work.0.join("extracted");
    let extracted_pack =
        safe_extract_zip(&archive_path, &extraction_root, &sidecar, installer, app)?;
    verify_pack(&extracted_pack, &sidecar, installer, app)?;
    installer.check_cancelled()?;

    emit_progress(
        app,
        Progress {
            phase: "installing",
            pct: 89,
            received: None,
            total: None,
            message: "Installing the verified runtime version",
        },
    );
    let installed =
        install_versioned_pack(&extracted_pack, &root, &sidecar, installer, app, &suffix)?;

    let candidate = root.join(format!(".current-next-{suffix}"));
    clone_tree(&installed, &candidate, installer)?;
    if let Err(error) = verify_pack(&candidate, &sidecar, installer, app) {
        remove_owned_path(&candidate);
        return Err(format!("activation copy failed verification: {error}"));
    }
    if let Err(error) = installer.check_cancelled() {
        remove_owned_path(&candidate);
        return Err(error);
    }
    installer.begin_commit()?;

    emit_progress(
        app,
        Progress {
            phase: "activating",
            pct: 94,
            received: None,
            total: None,
            message: "Activating the new runtime",
        },
    );
    let stack = app.state::<crate::stack::Stack>();
    crate::stack::stop_all(stack.inner());
    let mut activation = match activate_candidate(&root, &candidate, &suffix) {
        Ok(activation) => activation,
        Err(error) => {
            remove_owned_path(&candidate);
            let restart_detail = if installer.exiting.load(AtomicOrdering::Acquire) {
                "skipped because the application is shutting down".to_string()
            } else {
                crate::stack::start(app, stack.inner())
                    .err()
                    .unwrap_or_else(|| "started".to_string())
            };
            return Err(format!(
                "runtime activation failed: {error}; previous stack restart: {}",
                restart_detail
            ));
        }
    };

    if let Err(error) = installer.check_cancelled() {
        let had_previous = activation.has_previous();
        let rollback = activation.rollback(&suffix);
        let rollback_detail = match (had_previous, rollback) {
            (true, Ok(())) => "restored the previous runtime".to_string(),
            (false, Ok(())) => {
                "no previous runtime was available; retained the verified runtime".to_string()
            }
            (_, Err(rollback_error)) => format!("failed: {rollback_error}"),
        };
        return Err(format!(
            "runtime activation stopped before readiness: {error}; rollback: {rollback_detail}"
        ));
    }

    emit_progress(
        app,
        Progress {
            phase: "restarting",
            pct: 97,
            received: None,
            total: None,
            message: "Starting the local speech services",
        },
    );
    let startup_result = crate::stack::start(app, stack.inner())
        .and_then(|_| crate::stack::wait_until_ready(stack.inner(), Duration::from_secs(120)));
    if let Err(error) = startup_result {
        // `start` may have launched one child before the other failed. Close
        // every managed child before replacing the files they were using.
        crate::stack::stop_all(stack.inner());
        let had_previous = activation.has_previous();
        let rollback = activation.rollback(&suffix);
        let rollback_detail = match &rollback {
            Ok(()) if had_previous => "restored the previous runtime".to_string(),
            Ok(()) => {
                "no previous runtime was available; retained the verified runtime".to_string()
            }
            Err(rollback_error) => format!("failed: {rollback_error}"),
        };
        let restart_detail = if rollback.is_err() {
            "skipped because rollback failed".to_string()
        } else if !had_previous {
            "skipped because no previous runtime was available".to_string()
        } else if installer.exiting.load(AtomicOrdering::Acquire) {
            "skipped because the application is shutting down".to_string()
        } else {
            crate::stack::start(app, stack.inner())
                .err()
                .unwrap_or_else(|| "started".to_string())
        };
        return Err(format!(
            "runtime {} ({}) was installed but the speech services did not start: {error}; rollback: {rollback_detail}; previous stack restart: {restart_detail}",
            sidecar.raw.contract.version, sidecar.raw.contract.flavor,
        ));
    }
    if let Err(error) = activation.commit(&suffix) {
        crate::stack::stop_all(stack.inner());
        let rollback = activation.rollback(&suffix);
        let rollback_detail = rollback
            .as_ref()
            .err()
            .map(|rollback_error| format!("failed: {rollback_error}"))
            .unwrap_or_else(|| "restored the previous runtime".to_string());
        let restart_detail = if rollback.is_err() {
            "skipped because rollback failed".to_string()
        } else if installer.exiting.load(AtomicOrdering::Acquire) {
            "skipped because the application is shutting down".to_string()
        } else {
            crate::stack::start(app, stack.inner())
                .err()
                .unwrap_or_else(|| "started".to_string())
        };
        return Err(format!(
            "the new runtime became ready but its activation could not be committed: {error}; rollback: {rollback_detail}; previous stack restart: {restart_detail}"
        ));
    }
    emit_progress(
        app,
        Progress {
            phase: "ready",
            pct: 100,
            received: Some(sidecar.raw.archive.byte_size),
            total: Some(sidecar.raw.archive.byte_size),
            message: "Speech runtime installed",
        },
    );
    Ok(RuntimeInstallResult {
        version: sidecar.raw.contract.version,
        flavor: sidecar.raw.contract.flavor,
        installed_path: installed.to_string_lossy().to_string(),
        source: sidecar.source.to_string(),
    })
}

fn emit_terminal_error(app: &AppHandle, error: &str) {
    let cancelled = error.contains("cancelled");
    emit_progress(
        app,
        Progress {
            phase: if cancelled { "cancelled" } else { "error" },
            pct: 0,
            received: None,
            total: None,
            message: error,
        },
    );
}

#[tauri::command]
pub async fn runtime_install_plan(app: AppHandle) -> Result<RuntimeInstallPlan, String> {
    tauri::async_runtime::spawn_blocking(move || {
        #[cfg(not(windows))]
        {
            return Err("automatic runtime installation is currently Windows-only".to_string());
        }
        #[cfg(windows)]
        {
            let installer = app.state::<RuntimeInstaller>();
            let result = runtime_plan_blocking(&app, installer.inner());
            if let Err(error) = &result {
                emit_terminal_error(&app, error);
            }
            result
        }
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn install_runtime(
    app: AppHandle,
    plan_id: String,
) -> Result<RuntimeInstallResult, String> {
    tauri::async_runtime::spawn_blocking(move || {
        #[cfg(not(windows))]
        {
            return Err("automatic runtime installation is currently Windows-only".to_string());
        }
        #[cfg(windows)]
        {
            let installer = app.state::<RuntimeInstaller>();
            let result = install_runtime_blocking(&app, installer.inner(), &plan_id);
            if let Err(error) = &result {
                emit_terminal_error(&app, error);
            }
            result
        }
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
pub fn cancel_runtime_install(installer: tauri::State<RuntimeInstaller>) -> bool {
    installer.cancel()
}

pub fn cancel_on_exit(installer: &RuntimeInstaller) {
    installer.exiting.store(true, AtomicOrdering::Release);
    installer.cancel();
    let deadline = Instant::now() + Duration::from_secs(30);
    let mut activity = installer.activity.lock().unwrap();
    while activity.active {
        let now = Instant::now();
        if now >= deadline {
            break;
        }
        let (next, _) = installer
            .idle
            .wait_timeout(activity, deadline.saturating_duration_since(now))
            .unwrap();
        activity = next;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TestDirectory(PathBuf);

    impl TestDirectory {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "orpheus-runtime-installer-test-{}",
                unique_suffix()
            ));
            fs::create_dir(&path).unwrap();
            Self(path)
        }
    }

    impl Drop for TestDirectory {
        fn drop(&mut self) {
            remove_owned_path(&self.0);
        }
    }

    fn valid_contract() -> PackContract {
        PackContract {
            schema_version: MANIFEST_SCHEMA,
            version: "2026.07.18".to_string(),
            platform: "windows".to_string(),
            architecture: expected_architecture().to_string(),
            flavor: "cuda".to_string(),
            models_included: false,
            voice_models_included: false,
            decoder_assets: DecoderAssets {
                included: true,
                repo_id: "example/snac".to_string(),
                revision: "0123456789abcdef".to_string(),
                license: "MIT".to_string(),
                model_root: "backend/snac-model".to_string(),
                license_file: "backend/LICENSE-SNAC.txt".to_string(),
                byte_size: 10,
                files: vec![DecoderFile {
                    path: "config.json".to_string(),
                    byte_size: 10,
                    sha256: "c".repeat(64),
                }],
            },
            llama_server: "llama/llama-server.exe".to_string(),
            backend_exe: "backend/orpheus-backend.exe".to_string(),
            backend_dir: "backend".to_string(),
            backend_args: Vec::new(),
            payload: PayloadSummary {
                byte_size: 12,
                file_count: 2,
                sha256: "a".repeat(64),
                hash_format: HASH_FORMAT.to_string(),
            },
        }
    }

    fn valid_sidecar() -> SidecarManifest {
        SidecarManifest {
            contract: valid_contract(),
            archive: ArchiveRecord {
                file_name: "orpheus-runtime.zip".to_string(),
                byte_size: 100,
                sha256: "b".repeat(64),
                parts: Vec::new(),
            },
        }
    }

    #[test]
    fn cuda_runtime_requires_an_r580_or_newer_nvidia_driver() {
        assert!(!nvidia_driver_supports_cuda_13(""));
        assert!(!nvidia_driver_supports_cuda_13("572.61\n"));
        assert!(nvidia_driver_supports_cuda_13("580.00\n"));
        assert!(nvidia_driver_supports_cuda_13("591.86\n"));
        assert!(nvidia_driver_supports_cuda_13("572.61\n591.86\n"));
    }

    #[test]
    fn validates_sidecar_contract_and_same_origin_archive() {
        let manifest_url =
            Url::parse("https://downloads.example.test/releases/runtime.manifest.json").unwrap();
        let validated = validate_sidecar(valid_sidecar(), &manifest_url, "test").unwrap();
        assert_eq!(
            validated.archive_url.as_str(),
            "https://downloads.example.test/releases/orpheus-runtime.zip"
        );
        assert_eq!(validated.raw.contract.version, "2026.07.18");
        assert!(validated.part_urls.is_empty());
    }

    #[test]
    fn runtime_plan_id_binds_the_displayed_release_contract() {
        let manifest_url =
            Url::parse("https://downloads.example.test/releases/runtime.manifest.json").unwrap();
        let first = validate_sidecar(valid_sidecar(), &manifest_url, "test").unwrap();
        let first_id = runtime_plan_id(&first);

        let mut changed = valid_sidecar();
        changed.archive.byte_size += 1;
        changed.archive.sha256 = "d".repeat(64);
        let second = validate_sidecar(changed, &manifest_url, "test").unwrap();

        assert_ne!(first_id, runtime_plan_id(&second));
        assert!(first_id.starts_with("2026.07.18:cuda:100:"));
    }

    #[test]
    fn validates_multipart_archive_contract_and_ordered_urls() {
        let manifest_url =
            Url::parse("https://downloads.example.test/releases/runtime.manifest.json").unwrap();
        let mut sidecar = valid_sidecar();
        sidecar.archive.parts = vec![
            ArchivePart {
                file_name: "orpheus-runtime.zip.part001".to_string(),
                byte_size: 40,
                sha256: "c".repeat(64),
            },
            ArchivePart {
                file_name: "orpheus-runtime.zip.part002".to_string(),
                byte_size: 60,
                sha256: "d".repeat(64),
            },
        ];

        let validated = validate_sidecar(sidecar, &manifest_url, "test").unwrap();
        assert_eq!(validated.part_urls.len(), 2);
        assert_eq!(
            validated.part_urls[0].as_str(),
            "https://downloads.example.test/releases/orpheus-runtime.zip.part001"
        );
        assert_eq!(
            validated.part_urls[1].as_str(),
            "https://downloads.example.test/releases/orpheus-runtime.zip.part002"
        );
    }

    #[test]
    fn rejects_multipart_archive_size_and_name_mismatches() {
        let manifest_url =
            Url::parse("https://downloads.example.test/releases/runtime.manifest.json").unwrap();
        let mut wrong_size = valid_sidecar();
        wrong_size.archive.parts = vec![ArchivePart {
            file_name: "orpheus-runtime.zip.part001".to_string(),
            byte_size: 99,
            sha256: "c".repeat(64),
        }];
        let error = validate_sidecar(wrong_size, &manifest_url, "test")
            .err()
            .unwrap();
        assert!(error.contains("parts total"), "{error}");

        let mut duplicate = valid_sidecar();
        duplicate.archive.parts = vec![
            ArchivePart {
                file_name: "orpheus-runtime.zip.part001".to_string(),
                byte_size: 40,
                sha256: "c".repeat(64),
            },
            ArchivePart {
                file_name: "ORPHEUS-RUNTIME.ZIP.PART001".to_string(),
                byte_size: 60,
                sha256: "d".repeat(64),
            },
        ];
        let error = validate_sidecar(duplicate, &manifest_url, "test")
            .err()
            .unwrap();
        assert!(error.contains("case-colliding"), "{error}");
    }

    #[test]
    fn rejects_zip_and_manifest_path_traversal() {
        for path in [
            "../escape.exe",
            "runtime/../../escape.exe",
            "/absolute.exe",
            "C:/windows/system32/evil.exe",
            "runtime\\escape.exe",
            "runtime/con/payload.exe",
            "runtime/payload.exe. ",
        ] {
            assert!(
                validate_portable_file_path(path).is_err(),
                "accepted unsafe path {path:?}"
            );
        }
        assert!(!zip_mode_is_safe(Some(0o120777), false));
        assert!(zip_mode_is_safe(Some(0o100644), false));
    }

    #[test]
    fn rejects_machine_state_but_allows_runtime_templates_and_decoder_weight() {
        assert!(forbidden_payload_path("backend/.env"));
        assert!(forbidden_payload_path("backend/.ENV"));
        assert!(forbidden_payload_path("backend/stack.config.json"));
        assert!(forbidden_payload_path("backend/pyvenv.cfg"));
        assert!(!forbidden_payload_path("backend/.env.example"));
        assert!(!forbidden_payload_path(
            "backend/snac-model/pytorch_model.bin"
        ));
    }

    #[test]
    fn rejects_tampered_payload_file_hash() {
        let directory = TestDirectory::new();
        let file = directory.0.join("payload.dll");
        fs::write(&file, b"verified bytes").unwrap();
        let record = FileRecord {
            path: "payload.dll".to_string(),
            byte_size: 14,
            sha256: "0".repeat(64),
        };
        let error = verify_file_record(&file, &record, &RuntimeInstaller::default()).unwrap_err();
        assert!(error.contains("SHA-256 mismatch"), "{error}");
    }

    #[test]
    fn accepts_exact_payload_file_hash_and_size() {
        let directory = TestDirectory::new();
        let file = directory.0.join("payload.dll");
        fs::write(&file, b"verified bytes").unwrap();
        let mut hasher = Sha256::new();
        hasher.update(b"verified bytes");
        let record = FileRecord {
            path: "payload.dll".to_string(),
            byte_size: 14,
            sha256: format!("{:x}", hasher.finalize()),
        };
        verify_file_record(&file, &record, &RuntimeInstaller::default()).unwrap();
    }

    #[test]
    fn assembles_archive_parts_in_manifest_order() {
        let directory = TestDirectory::new();
        let first = directory.0.join("first.part");
        let second = directory.0.join("second.part");
        let assembled = directory.0.join("runtime.zip");
        fs::write(&first, b"ordered ").unwrap();
        fs::write(&second, b"archive").unwrap();
        let mut output = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&assembled)
            .unwrap();
        let installer = RuntimeInstaller::default();

        assert_eq!(append_file(&first, &mut output, &installer).unwrap(), 8);
        assert_eq!(append_file(&second, &mut output, &installer).unwrap(), 7);
        output.flush().unwrap();
        drop(output);

        assert_eq!(fs::read(assembled).unwrap(), b"ordered archive");
    }

    #[test]
    fn rejects_manifest_contract_drift() {
        let sidecar = valid_contract();
        let mut internal = valid_contract();
        internal.backend_args.push("--quiet".to_string());
        let error = contracts_match(&sidecar, &internal).unwrap_err();
        assert!(error.contains("does not match"));
    }

    #[test]
    fn activation_rollback_restores_previous_runtime() {
        let directory = TestDirectory::new();
        let current = directory.0.join("current");
        let candidate = directory.0.join("candidate");
        fs::create_dir(&current).unwrap();
        fs::create_dir(&candidate).unwrap();
        fs::write(current.join("runtime.txt"), b"previous").unwrap();
        fs::write(candidate.join("runtime.txt"), b"candidate").unwrap();

        let activation = activate_candidate(&directory.0, &candidate, "rollback").unwrap();
        assert_eq!(fs::read(current.join("runtime.txt")).unwrap(), b"candidate");
        assert!(activation.has_previous());
        activation.rollback("rollback").unwrap();

        assert_eq!(fs::read(current.join("runtime.txt")).unwrap(), b"previous");
        assert!(!directory.0.join(".current-previous-rollback").exists());
        assert!(!directory.0.join(".current-failed-rollback").exists());
    }

    #[test]
    fn activation_commit_discards_previous_runtime() {
        let directory = TestDirectory::new();
        let current = directory.0.join("current");
        let candidate = directory.0.join("candidate");
        fs::create_dir(&current).unwrap();
        fs::create_dir(&candidate).unwrap();
        fs::write(current.join("runtime.txt"), b"previous").unwrap();
        fs::write(candidate.join("runtime.txt"), b"candidate").unwrap();

        let mut activation = activate_candidate(&directory.0, &candidate, "commit").unwrap();
        activation.commit("commit").unwrap();

        assert_eq!(fs::read(current.join("runtime.txt")).unwrap(), b"candidate");
        assert!(!directory.0.join(".current-previous-commit").exists());
    }

    #[test]
    fn startup_recovers_an_activation_interrupted_before_commit() {
        let directory = TestDirectory::new();
        let current = directory.0.join("current");
        let candidate = directory.0.join("candidate");
        fs::create_dir(&current).unwrap();
        fs::create_dir(&candidate).unwrap();
        fs::write(current.join("runtime.txt"), b"previous").unwrap();
        fs::write(candidate.join("runtime.txt"), b"candidate").unwrap();

        let _pending = activate_candidate(&directory.0, &candidate, "crash").unwrap();
        recover_interrupted_activation(&directory.0).unwrap();

        assert_eq!(fs::read(current.join("runtime.txt")).unwrap(), b"previous");
        assert!(!directory.0.join(".current-previous-crash").exists());
        assert!(fs::read_dir(&directory.0).unwrap().all(|entry| {
            !entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with(".current-interrupted-")
        }));
    }
}
