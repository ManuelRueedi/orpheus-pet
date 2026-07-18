// Manages the Orpheus voice stack as child processes so the pet is ONE program
// (llama-server + Orpheus-FastAPI with the /v1/audio/speech/stream endpoint):
// launching the pet brings up llama-server (GGUF inference) and Orpheus-FastAPI
// (token -> WAV via SNAC), and quitting the pet tears them down again.
//
// Behaviour:
//   - Development reads the repo's stack.config.json; release keeps mutable
//     config, runtime packs, models, and logs in Tauri's per-user directories.
//     ORPHEUS_PET_CONFIG / ORPHEUS_PET_RUNTIME remain explicit overrides.
//   - If a port is already listening we DON'T spawn a duplicate — we reuse the
//     running server and, on quit, only kill what we spawned ourselves.
//   - Children run with hidden consoles; their output goes to logsDir.
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use std::{
    fs,
    io::{BufRead, BufReader, Read, Write},
    net::{SocketAddr, TcpStream},
    path::{Component, Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{
        atomic::{AtomicBool, Ordering},
        Mutex, TryLockError,
    },
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use tauri::{AppHandle, Emitter, Manager};

#[cfg(windows)]
use std::os::windows::process::CommandExt;

#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

const RUNTIME_MANIFEST_SCHEMA: u32 = 1;

#[derive(Clone)]
struct ManagedPaths {
    config_file: PathBuf,
    models_dir: PathBuf,
    cache_dir: PathBuf,
    logs_dir: PathBuf,
    runtime_dir: PathBuf,
}

impl ManagedPaths {
    fn resolve(app: &AppHandle) -> Result<Self, String> {
        let config_dir = app
            .path()
            .app_config_dir()
            .map_err(|e| format!("could not resolve app config directory: {e}"))?;
        let local_data_dir = app
            .path()
            .app_local_data_dir()
            .map_err(|e| format!("could not resolve app local-data directory: {e}"))?;
        let logs_dir = app
            .path()
            .app_log_dir()
            .map_err(|e| format!("could not resolve app log directory: {e}"))?;
        Ok(Self {
            config_file: config_dir.join("stack.config.json"),
            models_dir: local_data_dir.join("models"),
            cache_dir: local_data_dir.join("cache").join("huggingface"),
            logs_dir,
            runtime_dir: local_data_dir.join("runtime"),
        })
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RuntimeManifest {
    schema_version: u32,
    version: String,
    #[serde(default)]
    flavor: Option<String>,
    llama_server: String,
    backend_exe: String,
    #[serde(default)]
    backend_dir: Option<String>,
    #[serde(default)]
    backend_args: Vec<String>,
}

#[derive(Clone)]
struct RuntimeInfo {
    source: &'static str,
    manifest_path: Option<PathBuf>,
    version: Option<String>,
    flavor: Option<String>,
    llama_server: PathBuf,
    backend_dir: PathBuf,
    backend_exe: Option<PathBuf>,
    python: Option<PathBuf>,
    backend_args: Vec<String>,
    valid: bool,
}

impl RuntimeInfo {
    fn runtime_present(&self) -> bool {
        self.valid && self.llama_server.is_file()
    }

    fn backend_present(&self) -> bool {
        let launcher_present = self
            .backend_exe
            .as_ref()
            .or(self.python.as_ref())
            .is_some_and(|path| path.is_file());
        self.valid && self.backend_dir.is_dir() && launcher_present && self.decoder_present()
    }

    fn decoder_present(&self) -> bool {
        if self.source != "runtime-pack" {
            return true;
        }
        let decoder = self.backend_dir.join("snac-model");
        decoder.join("config.json").is_file() && decoder.join("pytorch_model.bin").is_file()
    }

    fn is_present(&self) -> bool {
        self.runtime_present() && self.backend_present()
    }
}

#[derive(Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct StackConfig {
    pub llama_server: String,
    pub model: String,
    #[serde(default = "d_llama_port")]
    pub llama_port: u16,
    #[serde(default)]
    pub llama_args: Vec<String>,
    pub orpheus_dir: String,
    pub python: String,
    #[serde(default = "d_orpheus_port")]
    pub orpheus_port: u16,
    pub logs_dir: Option<String>,
    /// Global "read my selection aloud" hotkey, e.g. "ctrl+alt+o".
    #[serde(default = "d_hotkey")]
    pub hotkey: String,
    /// Selected UI language code (en, fr, …); the model is derived from it.
    #[serde(default)]
    pub language: Option<String>,
    /// GGUF quantisation to fetch/use for the voice model: "Q8_0" (best
    /// quality), "Q4_K_M" (~⅔ the VRAM/RAM), or "Q2_K" (smallest). Lower quants
    /// keep the pet usable on lower-spec machines. Falls back to Q8_0 where a
    /// given quant isn't published for a language.
    #[serde(default = "d_quant")]
    pub quant: String,
}

fn d_llama_port() -> u16 {
    1234
}
fn d_orpheus_port() -> u16 {
    5005
}
fn d_hotkey() -> String {
    "ctrl+alt+o".into()
}
fn d_quant() -> String {
    "Q8_0".into()
}

const SUPPORTED_QUANTS: [&str; 3] = ["Q8_0", "Q4_K_M", "Q2_K"];

fn validate_quant(quant: &str) -> Result<(), String> {
    if SUPPORTED_QUANTS.contains(&quant) {
        Ok(())
    } else {
        Err(format!(
            "unsupported model size {quant:?}; expected {}",
            SUPPORTED_QUANTS.join(", ")
        ))
    }
}

fn unix_time_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelSwitchResult {
    pub operation_id: String,
    pub language: String,
    pub preferred_quant: String,
    pub loaded_quant: String,
    pub switched: bool,
}

#[derive(Default)]
struct LlamaProcessState {
    child: Option<Child>,
    reused: bool,
}

struct NamedProcess {
    name: String,
    child: Child,
}

#[derive(Default)]
struct ModelOperationState {
    active: bool,
    cancelled: bool,
    lifecycle_cancelled: bool,
    committing: bool,
    operation_id: Option<String>,
    phase: Option<String>,
    target_language: Option<String>,
    target_quant: Option<String>,
    started_at_ms: Option<u64>,
    download: Option<Child>,
}

struct ModelOperationGuard<'a> {
    stack: &'a Stack,
}

impl<'a> ModelOperationGuard<'a> {
    fn begin(
        stack: &'a Stack,
        operation_id: &str,
        language: &str,
        quant: &str,
    ) -> Result<Self, String> {
        let mut operation = stack.model_operation.lock().unwrap();
        if operation.active {
            return Err("another model switch is already in progress".to_string());
        }
        *operation = ModelOperationState {
            active: true,
            operation_id: Some(operation_id.to_string()),
            phase: Some("preparing".to_string()),
            target_language: Some(language.to_string()),
            target_quant: Some(quant.to_string()),
            started_at_ms: Some(unix_time_ms()),
            ..ModelOperationState::default()
        };
        drop(operation);
        Ok(Self { stack })
    }
}

impl Drop for ModelOperationGuard<'_> {
    fn drop(&mut self) {
        let download = {
            let mut operation = self.stack.model_operation.lock().unwrap();
            let child = operation.download.take();
            *operation = ModelOperationState::default();
            child
        };
        if let Some(child) = download {
            terminate_child(child);
        }
    }
}

#[derive(Default)]
pub struct Stack {
    // Serializes process/config/model lifecycle mutations. Status reads remain
    // independent, and cancellation never takes this lock so it can interrupt
    // a blocking download/load before restart or exit waits for the operation.
    lifecycle: Mutex<()>,
    lifecycle_interrupt: AtomicBool,
    processes: Mutex<Vec<NamedProcess>>,
    notes: Mutex<Vec<String>>,
    cfg: Mutex<Option<StackConfig>>,
    cfg_path: Mutex<Option<PathBuf>>,
    managed_paths: Mutex<Option<ManagedPaths>>,
    runtime: Mutex<Option<RuntimeInfo>>,
    hotkey_current: Mutex<Option<String>>,
    llama_process: Mutex<LlamaProcessState>,
    llama_port_conflict: Mutex<bool>,
    backend_port_conflict: Mutex<bool>,
    language: Mutex<Option<String>>,
    model_operation: Mutex<ModelOperationState>,
}

impl Stack {
    pub fn hotkey(&self) -> String {
        self.cfg
            .lock()
            .unwrap()
            .as_ref()
            .map(|c| c.hotkey.clone())
            .unwrap_or_else(d_hotkey)
    }

    pub fn registered_hotkey(&self) -> Option<String> {
        self.hotkey_current.lock().unwrap().clone()
    }

    pub fn set_registered_hotkey(&self, combo: String) {
        *self.hotkey_current.lock().unwrap() = Some(combo);
    }

    pub fn clear_registered_hotkey(&self) {
        *self.hotkey_current.lock().unwrap() = None;
    }

    // Write the chosen hotkey back to stack.config.json so it survives restart.
    pub fn persist_hotkey(&self, combo: &str) -> Result<(), String> {
        let _lifecycle = self.lifecycle.lock().unwrap();
        let path = self
            .cfg_path
            .lock()
            .unwrap()
            .clone()
            .ok_or_else(|| "config path unknown".to_string())?;
        let text = fs::read_to_string(&path).map_err(|e| e.to_string())?;
        let mut v: serde_json::Value = serde_json::from_str(&text).map_err(|e| e.to_string())?;
        v["hotkey"] = serde_json::Value::String(combo.to_string());
        let out = serde_json::to_string_pretty(&v).map_err(|e| e.to_string())?;
        fs::write(&path, out).map_err(|e| e.to_string())?;
        if let Some(c) = self.cfg.lock().unwrap().as_mut() {
            c.hotkey = combo.to_string();
        }
        Ok(())
    }

    pub fn current_language(&self) -> Option<String> {
        self.language.lock().unwrap().clone()
    }

    pub fn set_current_language(&self, lang: &str) {
        *self.language.lock().unwrap() = Some(lang.to_string());
    }

    pub fn current_quant(&self) -> String {
        self.cfg
            .lock()
            .unwrap()
            .as_ref()
            .map(|c| c.quant.clone())
            .unwrap_or_else(d_quant)
    }

    fn set_selection_state(&self, path: &Path, lang: &str, quant: &str) {
        if let Some(config) = self.cfg.lock().unwrap().as_mut() {
            config.model = path.to_string_lossy().to_string();
            config.language = Some(lang.to_string());
            config.quant = quant.to_string();
        }
        self.set_current_language(lang);
    }

    fn model_dir(&self) -> PathBuf {
        if let Some(paths) = self.managed_paths.lock().unwrap().as_ref() {
            return paths.models_dir.clone();
        }
        self.cfg
            .lock()
            .unwrap()
            .as_ref()
            .and_then(|c| Path::new(&c.model).parent().map(Path::to_path_buf))
            .unwrap_or_else(|| PathBuf::from("models"))
    }

    // Commit the model, language, and actual quant together only after the new
    // llama process has answered its health probe. A failed load therefore
    // leaves both the live voice and the saved selection on the previous model.
    fn persist_selection(&self, model_path: &Path, lang: &str, quant: &str) -> Result<(), String> {
        validate_quant(quant)?;
        if let Some(path) = self.cfg_path.lock().unwrap().clone() {
            let text = fs::read_to_string(&path).map_err(|e| e.to_string())?;
            let mut v: serde_json::Value =
                serde_json::from_str(&text).map_err(|e| e.to_string())?;
            v["model"] = serde_json::Value::String(model_path.to_string_lossy().to_string());
            v["language"] = serde_json::Value::String(lang.to_string());
            v["quant"] = serde_json::Value::String(quant.to_string());
            let out = serde_json::to_string_pretty(&v).map_err(|e| e.to_string())?;
            fs::write(&path, out).map_err(|e| e.to_string())?;
        }
        let orpheus_dir = self
            .cfg
            .lock()
            .unwrap()
            .as_ref()
            .map(|c| c.orpheus_dir.clone());
        if let Some(dir) = orpheus_dir {
            let env_path = PathBuf::from(dir).join(".env");
            if let Ok(text) = fs::read_to_string(&env_path) {
                let fname = model_path
                    .file_name()
                    .map(|f| f.to_string_lossy().to_string())
                    .unwrap_or_default();
                let mut lines: Vec<String> = text.lines().map(|l| l.to_string()).collect();
                let mut found = false;
                for l in lines.iter_mut() {
                    if l.trim_start().starts_with("ORPHEUS_MODEL_NAME=") {
                        *l = format!("ORPHEUS_MODEL_NAME={fname}");
                        found = true;
                        break;
                    }
                }
                if !found {
                    lines.push(format!("ORPHEUS_MODEL_NAME={fname}"));
                }
                let mut out = lines.join("\n");
                out.push('\n');
                let _ = fs::write(&env_path, out);
            }
        }
        Ok(())
    }
}

pub fn note(stack: &Stack, msg: String) {
    stack.notes.lock().unwrap().push(msg);
}

// The example config, baked into the binary so a first run with no
// stack.config.json can create one instead of failing (saves the manual copy).
const DEFAULT_CONFIG: &str = include_str!("../../stack.config.example.json");

// Where to write stack.config.json when none exists yet. In dev the process cwd
// is src-tauri, so the config belongs one level up at the app root (orpheus-pet/),
// where its relative paths ("../llama/…") resolve correctly. Release config is
// mutable user state, so it belongs in Tauri's per-user app-config directory.
fn default_config_target(managed: &ManagedPaths) -> Option<PathBuf> {
    if cfg!(debug_assertions) {
        std::env::current_dir()
            .ok()
            .and_then(|d| d.parent().map(|p| p.join("stack.config.json")))
    } else {
        Some(managed.config_file.clone())
    }
}

fn config_candidates(managed: &ManagedPaths) -> Vec<PathBuf> {
    let mut v = Vec::new();
    if let Ok(p) = std::env::var("ORPHEUS_PET_CONFIG") {
        v.push(PathBuf::from(p));
    }
    if cfg!(debug_assertions) {
        if let Ok(exe) = std::env::current_exe() {
            if let Some(dir) = exe.parent() {
                v.push(dir.join("stack.config.json"));
            }
        }
        if let Ok(cwd) = std::env::current_dir() {
            v.push(cwd.join("stack.config.json"));
            // In dev the process cwd is src-tauri; the config lives in the app root.
            v.push(cwd.join("..").join("stack.config.json"));
        }
    } else {
        v.push(managed.config_file.clone());
    }
    v
}

// Returns the parsed config, its path, and whether we just created it from the
// bundled default (true) vs. found an existing file (false).
fn load_config(managed: &ManagedPaths) -> Result<(StackConfig, PathBuf, bool), String> {
    let candidates = config_candidates(managed);
    for path in &candidates {
        if path.is_file() {
            let text = fs::read_to_string(path)
                .map_err(|e| format!("failed to read {}: {e}", path.display()))?;
            let cfg: StackConfig = serde_json::from_str(&text)
                .map_err(|e| format!("failed to parse {}: {e}", path.display()))?;
            validate_quant(&cfg.quant).map_err(|e| format!("invalid {}: {e}", path.display()))?;
            return Ok((cfg, path.clone(), false));
        }
    }
    // Nothing found: create stack.config.json from the baked-in example so a
    // fresh clone / first run works without the manual copy step. Parse first so
    // an invalid bundled default errors out instead of writing a broken file.
    if let Some(target) = default_config_target(managed) {
        let cfg: StackConfig = serde_json::from_str(DEFAULT_CONFIG)
            .map_err(|e| format!("bundled default config is invalid: {e}"))?;
        validate_quant(&cfg.quant)
            .map_err(|e| format!("bundled default config is invalid: {e}"))?;
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent)
                .map_err(|e| format!("couldn't create {}: {e}", parent.display()))?;
        }
        fs::write(&target, DEFAULT_CONFIG)
            .map_err(|e| format!("couldn't create {}: {e}", target.display()))?;
        return Ok((cfg, target, true));
    }
    Err(format!(
        "stack.config.json not found (searched: {})",
        candidates
            .iter()
            .map(|p| p.display().to_string())
            .collect::<Vec<_>>()
            .join(", ")
    ))
}

// Make a possibly-relative path absolute, resolved against `base` (the config
// file's directory). Lets a shared stack.config.json use "../models/…" paths
// that work on any machine, not just the one it was authored on.
fn abspath(base: &Path, s: &str) -> String {
    if s.is_empty() || Path::new(s).is_absolute() {
        s.to_string()
    } else {
        base.join(s).to_string_lossy().to_string()
    }
}

fn resolve_paths(cfg: &mut StackConfig, base: &Path) {
    cfg.llama_server = abspath(base, &cfg.llama_server);
    cfg.model = abspath(base, &cfg.model);
    cfg.orpheus_dir = abspath(base, &cfg.orpheus_dir);
    cfg.python = abspath(base, &cfg.python);
    if let Some(l) = cfg.logs_dir.clone() {
        cfg.logs_dir = Some(abspath(base, &l));
    }
}

fn apply_managed_paths(cfg: &mut StackConfig, managed: &ManagedPaths) {
    let model_name = Path::new(&cfg.model)
        .file_name()
        .filter(|name| !name.is_empty())
        .map(|name| name.to_os_string())
        .unwrap_or_else(|| model_file("Orpheus-3b-FT", &cfg.quant).into());
    cfg.model = managed
        .models_dir
        .join(model_name)
        .to_string_lossy()
        .to_string();
    cfg.logs_dir = Some(managed.logs_dir.to_string_lossy().to_string());
}

// Runtime pack paths must remain relative to the manifest directory. Besides
// keeping packs relocatable, rejecting `..` prevents a malformed manifest from
// launching an arbitrary executable elsewhere on the machine.
fn runtime_member(root: &Path, value: &str, field: &str) -> Result<PathBuf, String> {
    let path = Path::new(value);
    if value.trim().is_empty()
        || path.is_absolute()
        || path.components().any(|component| {
            matches!(
                component,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        })
    {
        return Err(format!(
            "runtime manifest {field} must be a non-empty relative path"
        ));
    }
    Ok(root.join(path))
}

fn runtime_from_manifest(
    manifest_path: &Path,
    manifest: RuntimeManifest,
) -> Result<RuntimeInfo, String> {
    if manifest.schema_version != RUNTIME_MANIFEST_SCHEMA {
        return Err(format!(
            "unsupported runtime manifest schema {} (expected {RUNTIME_MANIFEST_SCHEMA})",
            manifest.schema_version
        ));
    }
    if manifest.version.trim().is_empty() {
        return Err("runtime manifest version is empty".to_string());
    }
    let root = manifest_path
        .parent()
        .ok_or_else(|| "runtime manifest has no parent directory".to_string())?;
    let llama_server = runtime_member(root, &manifest.llama_server, "llamaServer")?;
    let backend_exe = runtime_member(root, &manifest.backend_exe, "backendExe")?;
    let backend_dir = match manifest.backend_dir.as_deref() {
        Some(dir) => runtime_member(root, dir, "backendDir")?,
        None => backend_exe
            .parent()
            .map(Path::to_path_buf)
            .ok_or_else(|| "runtime backend executable has no parent directory".to_string())?,
    };
    Ok(RuntimeInfo {
        source: "runtime-pack",
        manifest_path: Some(manifest_path.to_path_buf()),
        version: Some(manifest.version),
        flavor: manifest.flavor,
        llama_server,
        backend_dir,
        backend_exe: Some(backend_exe),
        python: None,
        backend_args: manifest.backend_args,
        valid: true,
    })
}

fn read_runtime_manifest(manifest_path: &Path) -> Result<RuntimeInfo, String> {
    let text = fs::read_to_string(manifest_path)
        .map_err(|e| format!("could not read {}: {e}", manifest_path.display()))?;
    let manifest: RuntimeManifest = serde_json::from_str(&text)
        .map_err(|e| format!("could not parse {}: {e}", manifest_path.display()))?;
    runtime_from_manifest(manifest_path, manifest)
}

fn dev_runtime(cfg: &StackConfig) -> RuntimeInfo {
    RuntimeInfo {
        source: "development-config",
        manifest_path: None,
        version: None,
        flavor: None,
        llama_server: PathBuf::from(&cfg.llama_server),
        backend_dir: PathBuf::from(&cfg.orpheus_dir),
        backend_exe: None,
        python: Some(PathBuf::from(&cfg.python)),
        backend_args: Vec::new(),
        valid: true,
    }
}

fn expected_runtime(manifest_path: PathBuf) -> RuntimeInfo {
    let root = manifest_path
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_default();
    RuntimeInfo {
        source: "runtime-pack",
        manifest_path: Some(manifest_path),
        version: None,
        flavor: None,
        llama_server: root.join("llama").join("llama-server.exe"),
        backend_dir: root.join("backend"),
        backend_exe: Some(root.join("backend").join("orpheus-backend.exe")),
        python: None,
        backend_args: Vec::new(),
        valid: false,
    }
}

fn runtime_manifest_path(managed: &ManagedPaths) -> PathBuf {
    if let Ok(value) = std::env::var("ORPHEUS_PET_RUNTIME") {
        let path = PathBuf::from(value);
        if path.extension().is_some_and(|ext| ext == "json") {
            return path;
        }
        return path.join("manifest.json");
    }
    managed.runtime_dir.join("current").join("manifest.json")
}

fn discover_runtime(
    cfg: &mut StackConfig,
    managed: &ManagedPaths,
    notes: &mut Vec<String>,
) -> RuntimeInfo {
    let explicit_runtime = std::env::var_os("ORPHEUS_PET_RUNTIME").is_some();
    if cfg!(debug_assertions) && !explicit_runtime {
        return dev_runtime(cfg);
    }

    let manifest_path = runtime_manifest_path(managed);
    match read_runtime_manifest(&manifest_path) {
        Ok(runtime) => {
            cfg.llama_server = runtime.llama_server.to_string_lossy().to_string();
            cfg.orpheus_dir = runtime.backend_dir.to_string_lossy().to_string();
            notes.push(format!(
                "runtime: {}{} from {}",
                runtime.version.as_deref().unwrap_or("unknown version"),
                runtime
                    .flavor
                    .as_deref()
                    .map(|flavor| format!(" ({flavor})"))
                    .unwrap_or_default(),
                manifest_path.display()
            ));
            runtime
        }
        Err(error) if cfg!(debug_assertions) => {
            notes.push(format!(
                "runtime override ignored: {error}; using the development stack"
            ));
            dev_runtime(cfg)
        }
        Err(error) => {
            notes.push(format!(
                "runtime unavailable: {error}. Install or repair the runtime pack at {}",
                manifest_path.display()
            ));
            expected_runtime(manifest_path)
        }
    }
}

fn port_open(port: u16) -> bool {
    let addr: SocketAddr = ([127, 0, 0, 1], port).into();
    TcpStream::connect_timeout(&addr, Duration::from_millis(300)).is_ok()
}

// A listening socket only says that *something* owns the port. These endpoints
// return 200 only after the actual local service has finished initialising; in
// particular llama.cpp's /health stays non-200 while its GGUF is loading.
fn http_endpoint_ready(port: u16, path: &str) -> bool {
    let addr: SocketAddr = ([127, 0, 0, 1], port).into();
    let Ok(mut stream) = TcpStream::connect_timeout(&addr, Duration::from_millis(300)) else {
        return false;
    };
    let timeout = Some(Duration::from_millis(500));
    let _ = stream.set_read_timeout(timeout);
    let _ = stream.set_write_timeout(timeout);
    let request =
        format!("GET {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n\r\n");
    if stream.write_all(request.as_bytes()).is_err() {
        return false;
    }
    let mut status = String::new();
    let Ok(read) = BufReader::new(stream).read_line(&mut status) else {
        return false;
    };
    if read == 0 {
        return false;
    }
    status.starts_with("HTTP/1.1 200") || status.starts_with("HTTP/1.0 200")
}

fn llama_ready(port: u16) -> bool {
    http_endpoint_ready(port, "/health")
}

fn backend_ready(port: u16) -> bool {
    http_endpoint_ready(port, "/v1/audio/voices")
}

fn hidden(cmd: &mut Command) {
    #[cfg(windows)]
    cmd.creation_flags(CREATE_NO_WINDOW);
}

// stdout/stderr for a child: a log file in logsDir, or null if unavailable.
fn stdio_logs(dir: &Option<String>, name: &str) -> (Stdio, Stdio) {
    if let Some(d) = dir {
        let p = PathBuf::from(d);
        if fs::create_dir_all(&p).is_ok() {
            if let Ok(f) = fs::File::create(p.join(name)) {
                if let Ok(f2) = f.try_clone() {
                    return (Stdio::from(f), Stdio::from(f2));
                }
            }
        }
    }
    (Stdio::null(), Stdio::null())
}

// Orpheus-FastAPI reads ORPHEUS_API_URL from its .env file, and load_dotenv
// OVERRIDES the process environment with it — so a stale .env silently points
// the TTS at the wrong inference server (exactly what a leftover :8080 entry
// did once). The pet owns the stack: before spawning, rewrite that one line to
// match the llama-server we manage. Other .env keys are left untouched.
fn enforce_orpheus_env(cfg: &StackConfig, notes: &mut Vec<String>) {
    let dir = PathBuf::from(&cfg.orpheus_dir);
    let env_path = dir.join(".env");
    let desired = format!(
        "ORPHEUS_API_URL=http://127.0.0.1:{}/v1/completions",
        cfg.llama_port
    );
    let current = fs::read_to_string(&env_path)
        .or_else(|_| fs::read_to_string(dir.join(".env.example")))
        .unwrap_or_default();
    let mut lines: Vec<String> = current.lines().map(|l| l.to_string()).collect();
    let mut found = false;
    let mut changed = false;
    for l in lines.iter_mut() {
        if l.trim_start().starts_with("ORPHEUS_API_URL=") {
            found = true;
            if l.trim() != desired {
                *l = desired.clone();
                changed = true;
            }
            break;
        }
    }
    if !found {
        lines.push(desired);
        changed = true;
    }
    if changed {
        let mut out = lines.join("\n");
        out.push('\n');
        match fs::write(&env_path, out) {
            Ok(()) => notes.push(format!(
                ".env: ORPHEUS_API_URL enforced -> 127.0.0.1:{}",
                cfg.llama_port
            )),
            Err(e) => notes.push(format!(".env: enforcement write FAILED: {e}")),
        }
    }
}

fn spawn_named(
    stack: &Stack,
    name: &str,
    cmd: &mut Command,
    notes: &mut Vec<String>,
) -> Result<(), String> {
    match cmd.spawn() {
        Ok(child) => {
            let pid = child.id();
            stack.processes.lock().unwrap().push(NamedProcess {
                name: name.to_string(),
                child,
            });
            notes.push(format!("{name}: spawned (pid {pid})"));
            Ok(())
        }
        Err(error) => {
            let message = format!("{name}: FAILED to spawn: {error}");
            notes.push(message.clone());
            Err(message)
        }
    }
}

fn terminate_child(mut child: Child) {
    // Child::kill targets the retained OS process handle, unlike taskkill by a
    // bare PID, so PID reuse can never terminate an unrelated process.
    let _ = child.kill();
    let _ = child.wait();
}

fn kill_llama(stack: &Stack) {
    let child = {
        let mut process = stack.llama_process.lock().unwrap();
        process.reused = false;
        process.child.take()
    };
    if let Some(child) = child {
        terminate_child(child);
    }
}

fn spawn_llama(stack: &Stack, cfg: &StackConfig, model_path: &Path) -> Result<(), String> {
    kill_llama(stack);
    let (out, err) = stdio_logs(&cfg.logs_dir, "llama-server.log");
    let mut cmd = Command::new(&cfg.llama_server);
    cmd.arg("-m")
        .arg(model_path)
        .args(["--host", "127.0.0.1", "--port", &cfg.llama_port.to_string()])
        .args(&cfg.llama_args)
        .stdin(Stdio::null())
        .stdout(out)
        .stderr(err);
    hidden(&mut cmd);
    match cmd.spawn() {
        Ok(child) => {
            let mut process = stack.llama_process.lock().unwrap();
            process.child = Some(child);
            process.reused = false;
            Ok(())
        }
        Err(error) => {
            let message = format!("llama-server spawn failed: {error}");
            note(stack, message.clone());
            Err(message)
        }
    }
}

#[derive(Debug)]
enum LlamaWaitError {
    Cancelled,
    Exited(String),
    Monitor(String),
    Timeout,
}

impl LlamaWaitError {
    fn message(&self) -> String {
        match self {
            Self::Cancelled => "cancelled".to_string(),
            Self::Exited(status) => {
                format!("llama-server exited before it became ready ({status})")
            }
            Self::Monitor(error) => format!("could not monitor llama-server: {error}"),
            Self::Timeout => "llama-server did not become ready before the timeout".to_string(),
        }
    }
}

fn managed_llama_alive(stack: &Stack) -> Result<bool, LlamaWaitError> {
    let mut process = stack.llama_process.lock().unwrap();
    let result = match process.child.as_mut() {
        Some(child) => child.try_wait(),
        None => {
            return Err(LlamaWaitError::Exited(
                "the managed process is no longer available".to_string(),
            ))
        }
    };
    match result {
        Ok(None) => Ok(true),
        Ok(Some(status)) => {
            process.child.take();
            Err(LlamaWaitError::Exited(status.to_string()))
        }
        Err(error) => Err(LlamaWaitError::Monitor(error.to_string())),
    }
}

fn wait_llama_ready<F>(
    stack: &Stack,
    port: u16,
    timeout: Duration,
    observe_cancel: bool,
    mut heartbeat: F,
) -> Result<(), LlamaWaitError>
where
    F: FnMut(u64),
{
    let start = Instant::now();
    let mut last_heartbeat_second = u64::MAX;
    while start.elapsed() < timeout {
        if observe_cancel && stack.model_operation.lock().unwrap().cancelled {
            return Err(LlamaWaitError::Cancelled);
        }
        managed_llama_alive(stack)?;
        if llama_ready(port) {
            // Do not accept a response from a process that exited while the
            // health request was in flight (or from an unrelated port owner).
            managed_llama_alive(stack)?;
            return Ok(());
        }
        let elapsed_ms = start.elapsed().as_millis().try_into().unwrap_or(u64::MAX);
        let elapsed_second = elapsed_ms / 1_000;
        if elapsed_second != last_heartbeat_second {
            last_heartbeat_second = elapsed_second;
            heartbeat(elapsed_ms);
        }
        std::thread::sleep(Duration::from_millis(500));
    }
    Err(LlamaWaitError::Timeout)
}

fn model_swap_failure(
    stack: &Stack,
    cfg: &StackConfig,
    previous_model: &Path,
    restore_previous: bool,
    failure: &str,
) -> String {
    kill_llama(stack);
    let (cancelled, lifecycle_cancelled) = {
        let operation = stack.model_operation.lock().unwrap();
        (operation.cancelled, operation.lifecycle_cancelled)
    };
    if lifecycle_cancelled {
        return "cancelled".to_string();
    }
    if !restore_previous || !previous_model.is_file() {
        return if cancelled {
            "cancelled".to_string()
        } else {
            failure.to_string()
        };
    }

    let freed = Instant::now();
    while port_open(cfg.llama_port) && freed.elapsed() < Duration::from_secs(10) {
        std::thread::sleep(Duration::from_millis(200));
    }
    if port_open(cfg.llama_port) {
        *stack.llama_port_conflict.lock().unwrap() = true;
        return format!(
            "{failure}; the previous voice could not be restored because port {} stayed occupied",
            cfg.llama_port
        );
    }

    if let Err(error) = spawn_llama(stack, cfg, previous_model) {
        return format!("{failure}; restoring the previous voice failed: {error}");
    }
    match wait_llama_ready(
        stack,
        cfg.llama_port,
        Duration::from_secs(60),
        false,
        |_| {},
    ) {
        Ok(()) if cancelled => "cancelled; the previous voice was restored".to_string(),
        Ok(()) => format!("{failure}; the previous voice was restored"),
        Err(error) => {
            kill_llama(stack);
            format!(
                "{failure}; the previous voice also failed to start: {}",
                error.message()
            )
        }
    }
}

pub fn start(app: &AppHandle, stack: &Stack) -> Result<(), String> {
    let _lifecycle = stack.lifecycle.lock().unwrap();
    let result = start_inner(app, stack);
    stack.lifecycle_interrupt.store(false, Ordering::Release);
    result
}

// Runtime activation is transactional only after the freshly launched pack has
// answered through its real HTTP endpoints. Ordinary app startup stays
// non-blocking; the installer calls this before discarding its rollback copy.
pub fn wait_until_ready(stack: &Stack, timeout: Duration) -> Result<(), String> {
    let cfg = stack
        .cfg
        .lock()
        .unwrap()
        .clone()
        .ok_or_else(|| "speech stack has no loaded configuration".to_string())?;
    let needs_llama = Path::new(&cfg.model).is_file();
    let deadline = Instant::now() + timeout;

    loop {
        let backend_is_ready = backend_ready(cfg.orpheus_port);
        let llama_is_ready = !needs_llama || llama_ready(cfg.llama_port);
        if backend_is_ready && llama_is_ready {
            return Ok(());
        }
        if stack.lifecycle_interrupt.load(Ordering::Acquire) {
            return Err("speech stack startup was interrupted".to_string());
        }
        if *stack.backend_port_conflict.lock().unwrap() {
            return Err(format!(
                "speech backend port {} is occupied by an unhealthy process",
                cfg.orpheus_port
            ));
        }
        if needs_llama && *stack.llama_port_conflict.lock().unwrap() {
            return Err(format!(
                "llama-server port {} is occupied by an unhealthy process",
                cfg.llama_port
            ));
        }

        let backend_failure = {
            let mut processes = stack.processes.lock().unwrap();
            processes
                .iter_mut()
                .find(|process| process.name == "orpheus-fastapi")
                .and_then(|process| match process.child.try_wait() {
                    Ok(Some(status)) => Some(format!(
                        "speech backend exited before it became ready ({status})"
                    )),
                    Err(error) => Some(format!("could not monitor speech backend: {error}")),
                    Ok(None) => None,
                })
        };
        if let Some(error) = backend_failure {
            return Err(error);
        }

        if needs_llama {
            let llama_failure = {
                let mut process = stack.llama_process.lock().unwrap();
                process
                    .child
                    .as_mut()
                    .and_then(|child| match child.try_wait() {
                        Ok(Some(status)) => Some(format!(
                            "llama-server exited before it became ready ({status})"
                        )),
                        Err(error) => Some(format!("could not monitor llama-server: {error}")),
                        Ok(None) => None,
                    })
            };
            if let Some(error) = llama_failure {
                return Err(error);
            }
        }

        if Instant::now() >= deadline {
            let mut missing = Vec::new();
            if !backend_is_ready {
                missing.push("speech backend");
            }
            if !llama_is_ready {
                missing.push("llama-server");
            }
            return Err(format!(
                "timed out waiting for {} readiness",
                missing.join(" and ")
            ));
        }
        thread::sleep(Duration::from_millis(200));
    }
}

fn start_inner(app: &AppHandle, stack: &Stack) -> Result<(), String> {
    // `start` is also the repair/retry path. Keep status diagnostics about the
    // current attempt instead of accumulating stale errors across retries.
    stack.notes.lock().unwrap().clear();
    *stack.cfg.lock().unwrap() = None;
    *stack.cfg_path.lock().unwrap() = None;
    *stack.managed_paths.lock().unwrap() = None;
    *stack.runtime.lock().unwrap() = None;
    *stack.language.lock().unwrap() = None;
    *stack.llama_process.lock().unwrap() = LlamaProcessState::default();
    *stack.llama_port_conflict.lock().unwrap() = false;
    *stack.backend_port_conflict.lock().unwrap() = false;
    *stack.model_operation.lock().unwrap() = ModelOperationState::default();
    let mut notes = Vec::new();
    let mut startup_errors = Vec::new();
    let managed = match ManagedPaths::resolve(app) {
        Ok(paths) => paths,
        Err(error) => {
            let message = format!("path setup failed: {error}. The voice stack was not started");
            stack.notes.lock().unwrap().push(message.clone());
            return Err(message);
        }
    };
    *stack.managed_paths.lock().unwrap() = if cfg!(debug_assertions) {
        None
    } else {
        Some(managed.clone())
    };

    let (mut cfg, cfg_path, created) = match load_config(&managed) {
        Ok(v) => v,
        Err(e) => {
            let message = format!("config error: {e} — the voice stack was not started");
            stack.notes.lock().unwrap().push(message.clone());
            return Err(message);
        }
    };
    if created {
        notes.push(format!(
            "config: created {} from the bundled default — edit it to taste",
            cfg_path.display()
        ));
    }
    // Resolve relative paths against the config file's own directory so a
    // shared config ("../models/…") works wherever the repo is cloned.
    let base = cfg_path
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| PathBuf::from("."));
    resolve_paths(&mut cfg, &base);
    if !cfg!(debug_assertions) {
        apply_managed_paths(&mut cfg, &managed);
        for (label, dir) in [
            ("model", &managed.models_dir),
            ("cache", &managed.cache_dir),
            ("log", &managed.logs_dir),
        ] {
            if let Err(error) = fs::create_dir_all(dir) {
                notes.push(format!(
                    "{label} directory unavailable at {}: {error}",
                    dir.display()
                ));
            }
        }
    }
    let runtime = discover_runtime(&mut cfg, &managed, &mut notes);
    *stack.cfg.lock().unwrap() = Some(cfg.clone());
    *stack.cfg_path.lock().unwrap() = Some(cfg_path);
    *stack.runtime.lock().unwrap() = Some(runtime.clone());

    // 1. llama-server (GGUF model inference on the GPU)
    if llama_ready(cfg.llama_port) {
        stack.llama_process.lock().unwrap().reused = true;
        notes.push(format!(
            "llama-server: already running on :{}, reusing",
            cfg.llama_port
        ));
    } else if port_open(cfg.llama_port) {
        *stack.llama_port_conflict.lock().unwrap() = true;
        notes.push(format!(
            "llama-server: port :{} is occupied by an unhealthy or unrelated process",
            cfg.llama_port
        ));
    } else if Path::new(&cfg.model).is_file() && runtime.valid && runtime.llama_server.is_file() {
        match spawn_llama(stack, &cfg, Path::new(&cfg.model)) {
            Ok(()) => notes.push("llama-server: spawned".to_string()),
            Err(error) => startup_errors.push(error),
        }
    } else if Path::new(&cfg.model).is_file() {
        notes.push(format!(
            "llama-server: model is installed, but the runtime executable is unavailable at {} — repair the runtime pack",
            runtime.llama_server.display()
        ));
    } else {
        // Fresh clone with no model yet: don't spawn a doomed llama-server.
        // Picking a language in the panel downloads a model and starts it.
        notes.push(format!(
            "llama-server: no model at {} — pick a language in the panel to download one",
            cfg.model
        ));
    }

    // A configured preference is not a loaded language. Keep this empty until a
    // model really exists; the panel uses the OS locale for initial UI focus.
    *stack.language.lock().unwrap() = if Path::new(&cfg.model).is_file() {
        Some(
            cfg.language
                .clone()
                .unwrap_or_else(|| lang_for_model_file(&cfg.model).to_string()),
        )
    } else {
        None
    };

    // 2. Orpheus-FastAPI (uvicorn without --reload: single process, clean kill)
    if runtime.valid {
        enforce_orpheus_env(&cfg, &mut notes);
    }
    if backend_ready(cfg.orpheus_port) {
        notes.push(format!(
            "orpheus-fastapi: already running on :{}, reusing",
            cfg.orpheus_port
        ));
    } else if port_open(cfg.orpheus_port) {
        *stack.backend_port_conflict.lock().unwrap() = true;
        notes.push(format!(
            "orpheus-fastapi: port :{} is occupied by an unhealthy or unrelated process",
            cfg.orpheus_port
        ));
    } else if !runtime.valid {
        notes.push(
            "orpheus backend: runtime pack is not installed — install or repair it, then restart"
                .to_string(),
        );
    } else if !runtime.backend_present() {
        notes.push(format!(
            "orpheus backend or offline SNAC decoder files are incomplete under {} — repair the runtime pack",
            runtime.backend_dir.display()
        ));
    } else {
        let (out, err) = stdio_logs(&cfg.logs_dir, "orpheus-fastapi.log");
        let mut cmd = if let Some(exe) = runtime.backend_exe.as_ref() {
            let mut command = Command::new(exe);
            command.args(&runtime.backend_args);
            command
        } else if let Some(python) = runtime.python.as_ref() {
            let mut command = Command::new(python);
            command.args([
                "-m",
                "uvicorn",
                "app:app",
                "--host",
                "127.0.0.1",
                "--port",
                &cfg.orpheus_port.to_string(),
            ]);
            command
        } else {
            notes.push(
                "orpheus backend: runtime manifest does not define a launcher — repair the runtime pack"
                    .to_string(),
            );
            stack.notes.lock().unwrap().extend(notes);
            return Err("runtime manifest does not define a backend launcher".to_string());
        };
        if runtime
            .backend_exe
            .as_ref()
            .or(runtime.python.as_ref())
            .is_some_and(|path| path.is_file())
            && runtime.backend_dir.is_dir()
        {
            cmd.current_dir(&runtime.backend_dir)
                .env("PYTHONUTF8", "1") // its emoji log lines crash on cp1252 otherwise
                .env("ORPHEUS_HOST", "127.0.0.1")
                .env("ORPHEUS_PORT", cfg.orpheus_port.to_string());
            if !cfg!(debug_assertions) {
                cmd.env("HF_HOME", &managed.cache_dir);
            }
            if runtime.source == "runtime-pack" {
                cmd.env("ORPHEUS_SNAC_MODEL", runtime.backend_dir.join("snac-model"))
                    .env("HF_HUB_OFFLINE", "1");
            }
            cmd.stdin(Stdio::null()).stdout(out).stderr(err);
            hidden(&mut cmd);
            if let Err(error) = spawn_named(stack, "orpheus-fastapi", &mut cmd, &mut notes) {
                startup_errors.push(error);
            }
        }
    }

    stack.notes.lock().unwrap().extend(notes);
    if startup_errors.is_empty() {
        Ok(())
    } else {
        Err(startup_errors.join("; "))
    }
}

// Kill everything we spawned (whole process trees). Servers we merely reused
// are left alone.
pub fn stop_all(stack: &Stack) {
    stack.lifecycle_interrupt.store(true, Ordering::Release);
    cancel_model_operation(stack);
    let _lifecycle = stack.lifecycle.lock().unwrap();
    stop_all_inner(stack);
}

fn stop_all_inner(stack: &Stack) {
    kill_llama(stack);
    let processes: Vec<NamedProcess> = stack.processes.lock().unwrap().drain(..).collect();
    for process in processes {
        terminate_child(process.child);
    }
}

pub fn restart(app: &AppHandle, stack: &Stack) -> Result<(), String> {
    stack.lifecycle_interrupt.store(true, Ordering::Release);
    cancel_model_operation(stack);
    let _lifecycle = stack.lifecycle.lock().unwrap();
    stop_all_inner(stack);
    let result = start_inner(app, stack);
    stack.lifecycle_interrupt.store(false, Ordering::Release);
    result
}

// ---------------------------------------------------------------------------
// Per-language models: download on demand + hot-swap llama-server.
// Each Orpheus language is a SEPARATE GGUF fine-tune (see the project README);
// the base Orpheus-3b-FT model is English-only, so other languages need their
// own model loaded to sound right.
// ---------------------------------------------------------------------------
// Base model name per language (base Orpheus-3b-FT is English-only, so each
// language is its own fine-tune). Combine with a quant to get the filename.
fn model_base_for_lang(lang: &str) -> Option<&'static str> {
    Some(match lang {
        "en" => "Orpheus-3b-FT",
        "fr" => "Orpheus-3b-French-FT",
        "de" => "Orpheus-3b-German-FT",
        "ko" => "Orpheus-3b-Korean-FT",
        "hi" => "Orpheus-3b-Hindi-FT",
        "zh" => "Orpheus-3b-Chinese-FT",
        "es" | "it" => "Orpheus-3b-Italian_Spanish-FT",
        _ => return None,
    })
}

// Filename for a base + quant, e.g. ("Orpheus-3b-FT", "Q4_K_M").
fn model_file(base: &str, quant: &str) -> String {
    format!("{base}-{quant}.gguf")
}

// Rough download size (bytes) per quant, for the confirm dialog shown before we
// hit the network. Real size is read from disk once present.
fn est_size(quant: &str) -> u64 {
    match quant {
        "Q2_K" => 1_600_000_000,
        "Q3_K_M" | "Q3_K_L" => 1_900_000_000,
        "Q4_K_M" | "Q4_K_S" | "Q4_0" => 2_360_000_000,
        "Q5_K_M" | "Q5_K_S" => 2_700_000_000,
        "Q6_K" => 3_100_000_000,
        _ => 3_516_430_784, // Q8_0 and anything unknown
    }
}

fn lang_for_model_file(name: &str) -> &'static str {
    let n = name.to_lowercase();
    if n.contains("french") {
        "fr"
    } else if n.contains("german") {
        "de"
    } else if n.contains("korean") {
        "ko"
    } else if n.contains("hindi") {
        "hi"
    } else if n.contains("chinese") || n.contains("mandarin") {
        "zh"
    } else if n.contains("italian") || n.contains("spanish") {
        "es"
    } else {
        "en"
    }
}

fn hf_url(file: &str) -> String {
    format!("https://huggingface.co/lex-au/{file}/resolve/main/{file}")
}

const MIN_GGUF_BYTES: u64 = 100_000_000;

fn quant_for_model_path(path: &Path) -> Option<String> {
    let name = path.file_name()?.to_string_lossy();
    SUPPORTED_QUANTS
        .iter()
        .find(|quant| name.ends_with(&format!("-{quant}.gguf")))
        .map(|quant| (*quant).to_string())
}

fn validate_gguf_file(path: &Path, minimum_bytes: u64) -> Result<(), String> {
    let size = fs::metadata(path)
        .map_err(|error| format!("could not inspect {}: {error}", path.display()))?
        .len();
    if size < minimum_bytes {
        return Err(format!(
            "downloaded model is unexpectedly small ({size} bytes; expected at least {minimum_bytes})"
        ));
    }
    let mut file = fs::File::open(path)
        .map_err(|error| format!("could not open {}: {error}", path.display()))?;
    let mut magic = [0u8; 4];
    file.read_exact(&mut magic)
        .map_err(|error| format!("could not read {}: {error}", path.display()))?;
    if &magic != b"GGUF" {
        return Err("downloaded file is not a GGUF model".to_string());
    }
    Ok(())
}

fn validate_operation_id(operation_id: &str) -> Result<(), String> {
    if operation_id.trim().is_empty() {
        return Err("model switch operationId is required".to_string());
    }
    if operation_id.len() > 128 || operation_id.chars().any(char::is_control) {
        return Err("model switch operationId is invalid".to_string());
    }
    Ok(())
}

fn emit_model_progress(
    app: &AppHandle,
    stack: &Stack,
    operation_id: &str,
    lang: &str,
    quant: &str,
    phase: &str,
    extra: impl IntoIterator<Item = (&'static str, Value)>,
) {
    {
        let mut operation = stack.model_operation.lock().unwrap();
        if operation.operation_id.as_deref() == Some(operation_id) {
            operation.phase = Some(phase.to_string());
            operation.target_language = Some(lang.to_string());
            operation.target_quant = Some(quant.to_string());
        }
    }
    let mut payload = Map::new();
    payload.insert(
        "operationId".to_string(),
        Value::String(operation_id.to_string()),
    );
    payload.insert("lang".to_string(), Value::String(lang.to_string()));
    payload.insert("quant".to_string(), Value::String(quant.to_string()));
    payload.insert("phase".to_string(), Value::String(phase.to_string()));
    for (key, value) in extra {
        payload.insert(key.to_string(), value);
    }
    let _ = app.emit("model-progress", Value::Object(payload));
}

fn operation_cancelled(stack: &Stack) -> bool {
    stack.model_operation.lock().unwrap().cancelled
}

fn begin_model_commit(stack: &Stack) -> Result<(), String> {
    let mut operation = stack.model_operation.lock().unwrap();
    if operation.cancelled {
        return Err("cancelled".to_string());
    }
    operation.committing = true;
    Ok(())
}

#[derive(Debug)]
enum ModelDownloadError {
    Cancelled,
    Unavailable,
    Failed(String),
}

impl ModelDownloadError {
    fn message(self) -> String {
        match self {
            Self::Cancelled => "cancelled".to_string(),
            Self::Unavailable => "the requested model size is not published".to_string(),
            Self::Failed(message) => message,
        }
    }
}

// A single cancellable GET replaces the old HEAD+GET pair. The transfer has a
// bounded connection time and aborts a stalled connection, while still allowing
// several hours for a healthy multi-gigabyte download.
fn download_model(
    app: &AppHandle,
    stack: &Stack,
    operation_id: &str,
    lang: &str,
    quant: &str,
    url: &str,
    dest: &Path,
) -> Result<(), ModelDownloadError> {
    let total = est_size(quant);
    let part = dest.with_extension("part");
    let _ = fs::remove_file(&part);
    if operation_cancelled(stack) {
        return Err(ModelDownloadError::Cancelled);
    }
    emit_model_progress(
        app,
        stack,
        operation_id,
        lang,
        quant,
        "download",
        [
            ("pct", json!(0u64)),
            ("received", json!(0u64)),
            ("total", json!(total)),
        ],
    );

    let mut cmd = Command::new("curl");
    cmd.args([
        "-L",
        "--fail",
        "--connect-timeout",
        "10",
        "--retry",
        "2",
        "--retry-delay",
        "1",
        "--retry-connrefused",
        "--speed-limit",
        "1024",
        "--speed-time",
        "30",
        "--max-time",
        "21600",
        "-o",
    ])
    .arg(&part)
    .arg(url)
    .stdin(Stdio::null())
    .stdout(Stdio::null())
    .stderr(Stdio::null());
    hidden(&mut cmd);
    let child = cmd
        .spawn()
        .map_err(|error| ModelDownloadError::Failed(format!("curl spawn failed: {error}")))?;
    let cancelled_child = {
        let mut operation = stack.model_operation.lock().unwrap();
        operation.download = Some(child);
        if operation.cancelled {
            operation.download.take()
        } else {
            None
        }
    };
    if let Some(child) = cancelled_child {
        terminate_child(child);
        let _ = fs::remove_file(&part);
        return Err(ModelDownloadError::Cancelled);
    }

    loop {
        let wait_result = {
            let mut operation = stack.model_operation.lock().unwrap();
            if operation.cancelled {
                let child = operation.download.take();
                drop(operation);
                if let Some(child) = child {
                    terminate_child(child);
                }
                let _ = fs::remove_file(&part);
                return Err(ModelDownloadError::Cancelled);
            }
            let result = match operation.download.as_mut() {
                Some(child) => child.try_wait(),
                None => {
                    return Err(ModelDownloadError::Failed(
                        "download process disappeared".to_string(),
                    ))
                }
            };
            if result.is_err() || matches!(result, Ok(Some(_))) {
                operation.download.take();
            }
            result
        };
        match wait_result {
            Ok(Some(status)) if status.success() => {
                if let Err(error) = validate_gguf_file(&part, MIN_GGUF_BYTES) {
                    let _ = fs::remove_file(&part);
                    return Err(ModelDownloadError::Failed(error));
                }
                fs::rename(&part, dest).map_err(|error| {
                    ModelDownloadError::Failed(format!("could not finish model download: {error}"))
                })?;
                emit_model_progress(
                    app,
                    stack,
                    operation_id,
                    lang,
                    quant,
                    "download",
                    [
                        ("pct", json!(100u64)),
                        ("received", json!(total)),
                        ("total", json!(total)),
                    ],
                );
                return Ok(());
            }
            Ok(Some(status)) if status.code() == Some(22) => {
                let _ = fs::remove_file(&part);
                return Err(ModelDownloadError::Unavailable);
            }
            Ok(Some(status)) => {
                let _ = fs::remove_file(&part);
                return Err(ModelDownloadError::Failed(format!(
                    "download failed (curl exit {:?})",
                    status.code()
                )));
            }
            Ok(None) => {
                let received = fs::metadata(&part)
                    .map(|metadata| metadata.len())
                    .unwrap_or(0);
                let pct = received
                    .saturating_mul(100)
                    .checked_div(total)
                    .unwrap_or(0)
                    .min(99);
                emit_model_progress(
                    app,
                    stack,
                    operation_id,
                    lang,
                    quant,
                    "download",
                    [
                        ("pct", json!(pct)),
                        ("received", json!(received)),
                        ("total", json!(total)),
                    ],
                );
                std::thread::sleep(Duration::from_millis(600));
            }
            Err(error) => {
                let _ = fs::remove_file(&part);
                return Err(ModelDownloadError::Failed(format!(
                    "curl wait error: {error}"
                )));
            }
        }
    }
}

// User cancellation remains valid through model loading. If the destructive
// swap has begun, the switch path restores the previous model before returning.
pub fn cancel_download(stack: &Stack) -> bool {
    let download = {
        let mut operation = stack.model_operation.lock().unwrap();
        if !operation.active || operation.committing {
            return false;
        }
        operation.cancelled = true;
        operation.download.take()
    };
    if let Some(child) = download {
        terminate_child(child);
    }
    true
}

fn cancel_model_operation(stack: &Stack) {
    let download = {
        let mut operation = stack.model_operation.lock().unwrap();
        operation.cancelled = true;
        operation.lifecycle_cancelled = true;
        operation.download.take()
    };
    if let Some(child) = download {
        terminate_child(child);
    }
}

// Whether the model for `lang` is already downloaded, and its (estimated) size.
// `quant` lets the panel ask about a size OTHER than the loaded one (e.g. "is the
// smaller model already downloaded?" before offering to switch). None = the
// current config quant, so existing callers are unaffected.
#[tauri::command]
pub fn model_status(
    state: tauri::State<'_, Stack>,
    lang: String,
    quant: Option<String>,
) -> Result<serde_json::Value, String> {
    let quant = {
        let guard = state.cfg.lock().unwrap();
        quant
            .or_else(|| guard.as_ref().map(|c| c.quant.clone()))
            .unwrap_or_else(d_quant)
    };
    validate_quant(&quant)?;
    let dir = state.model_dir();
    Ok(match model_base_for_lang(&lang) {
        Some(base) => {
            let path = dir.join(model_file(base, &quant));
            let present = path.is_file();
            let size = if present {
                fs::metadata(&path)
                    .map(|m| m.len())
                    .unwrap_or_else(|_| est_size(&quant))
            } else {
                est_size(&quant)
            };
            json!({ "present": present, "sizeBytes": size, "supported": true })
        }
        None => json!({ "present": false, "sizeBytes": 0u64, "supported": false }),
    })
}

fn reject_model_switch(
    app: &AppHandle,
    stack: &Stack,
    operation_id: &str,
    lang: &str,
    quant: &str,
    error: String,
) -> String {
    emit_model_progress(
        app,
        stack,
        operation_id,
        lang,
        quant,
        "failed",
        [("message", json!(error.clone()))],
    );
    error
}

fn execute_model_switch(
    app: &AppHandle,
    stack: &Stack,
    operation_id: &str,
    lang: &str,
    quant: &str,
) -> Result<ModelSwitchResult, String> {
    let _operation = match ModelOperationGuard::begin(stack, operation_id, lang, quant) {
        Ok(operation) => operation,
        Err(error) => {
            return Err(reject_model_switch(
                app,
                stack,
                operation_id,
                lang,
                quant,
                error,
            ))
        }
    };
    emit_model_progress(app, stack, operation_id, lang, quant, "preparing", []);
    let result = model_switch_inner(app, stack, operation_id, lang, quant);
    if let Err(error) = &result {
        let (cancelled, actual_quant) = {
            let operation = stack.model_operation.lock().unwrap();
            (
                operation.cancelled,
                operation
                    .target_quant
                    .clone()
                    .unwrap_or_else(|| quant.to_string()),
            )
        };
        emit_model_progress(
            app,
            stack,
            operation_id,
            lang,
            &actual_quant,
            if cancelled { "cancelled" } else { "failed" },
            [("message", json!(error))],
        );
    }
    result
}

fn commit_model_selection(
    stack: &Stack,
    model_path: &Path,
    lang: &str,
    quant: &str,
) -> Result<(), String> {
    begin_model_commit(stack)?;
    stack.persist_selection(model_path, lang, quant)?;
    stack.set_selection_state(model_path, lang, quant);
    Ok(())
}

fn model_switch_inner(
    app: &AppHandle,
    stack: &Stack,
    operation_id: &str,
    lang: &str,
    requested_quant: &str,
) -> Result<ModelSwitchResult, String> {
    let cfg = stack
        .cfg
        .lock()
        .unwrap()
        .clone()
        .ok_or_else(|| "stack not started".to_string())?;
    let runtime =
        stack.runtime.lock().unwrap().clone().ok_or_else(|| {
            "speech runtime status is unavailable; restart Orpheus Pet".to_string()
        })?;
    if !runtime.is_present() {
        return Err(format!(
            "speech runtime is missing or incomplete at {}; install or repair it before loading a voice",
            runtime
                .manifest_path
                .as_deref()
                .and_then(Path::parent)
                .unwrap_or(&runtime.backend_dir)
                .display()
        ));
    }
    if stack.llama_process.lock().unwrap().reused {
        return Err(
            "the active llama-server is externally managed; stop it and restart Orpheus Pet before changing the loaded voice"
                .to_string(),
        );
    }
    let base = model_base_for_lang(lang).ok_or_else(|| format!("unsupported language: {lang}"))?;
    let dir = stack.model_dir();
    fs::create_dir_all(&dir)
        .map_err(|e| format!("could not create model directory {}: {e}", dir.display()))?;

    validate_quant(requested_quant)?;
    let mut actual_quant = requested_quant.to_string();
    let mut file = model_file(base, &actual_quant);
    let mut dest = dir.join(&file);
    let current = PathBuf::from(&cfg.model);

    // Attempt the requested artifact directly. A 404/HTTP failure reported by
    // curl exit 22 means that quant is unpublished; only then fall back to Q8.
    if !dest.is_file() {
        match download_model(
            app,
            stack,
            operation_id,
            lang,
            &actual_quant,
            &hf_url(&file),
            &dest,
        ) {
            Ok(()) => {}
            Err(ModelDownloadError::Unavailable) if requested_quant != "Q8_0" => {
                note(
                    stack,
                    format!("{requested_quant} unavailable for {lang}; using Q8_0"),
                );
                actual_quant = "Q8_0".to_string();
                file = model_file(base, &actual_quant);
                dest = dir.join(&file);
                emit_model_progress(
                    app,
                    stack,
                    operation_id,
                    lang,
                    &actual_quant,
                    "preparing",
                    [
                        ("fallbackFrom", json!(requested_quant)),
                        (
                            "message",
                            json!(format!("{requested_quant} is unavailable; using Q8_0")),
                        ),
                    ],
                );
                if !dest.is_file() {
                    download_model(
                        app,
                        stack,
                        operation_id,
                        lang,
                        &actual_quant,
                        &hf_url(&file),
                        &dest,
                    )
                    .map_err(ModelDownloadError::message)?;
                }
            }
            Err(error) => return Err(error.message()),
        }
    }
    validate_gguf_file(&dest, MIN_GGUF_BYTES)?;

    // Spanish and Italian share one GGUF. A language-only change (or selecting
    // the already loaded size) commits metadata without restarting llama.
    if dest == current
        && llama_ready(cfg.llama_port)
        && matches!(managed_llama_alive(stack), Ok(true))
    {
        commit_model_selection(stack, &dest, lang, &actual_quant)?;
        let mut ready_fields = vec![("switched", json!(false))];
        if actual_quant != requested_quant {
            ready_fields.push(("fallbackFrom", json!(requested_quant)));
            ready_fields.push((
                "message",
                json!(format!(
                    "{requested_quant} is unavailable; kept the loaded {actual_quant} model"
                )),
            ));
        }
        emit_model_progress(
            app,
            stack,
            operation_id,
            lang,
            &actual_quant,
            "ready",
            ready_fields,
        );
        return Ok(ModelSwitchResult {
            operation_id: operation_id.to_string(),
            language: lang.to_string(),
            preferred_quant: actual_quant.clone(),
            loaded_quant: actual_quant,
            switched: false,
        });
    }

    // Swap llama-server to the new model.
    emit_model_progress(
        app,
        stack,
        operation_id,
        lang,
        &actual_quant,
        "loading",
        [("elapsedMs", json!(0u64))],
    );
    let restore_previous = current.is_file()
        && llama_ready(cfg.llama_port)
        && matches!(managed_llama_alive(stack), Ok(true));
    if operation_cancelled(stack) {
        return Err("cancelled".to_string());
    }
    kill_llama(stack);
    let freed = Instant::now();
    while port_open(cfg.llama_port) && freed.elapsed() < Duration::from_secs(10) {
        std::thread::sleep(Duration::from_millis(200));
    }
    if port_open(cfg.llama_port) {
        *stack.llama_port_conflict.lock().unwrap() = true;
        let failure = format!(
            "llama-server port {} is still occupied; close the other process and retry",
            cfg.llama_port
        );
        return Err(model_swap_failure(
            stack,
            &cfg,
            &current,
            restore_previous,
            &failure,
        ));
    }
    *stack.llama_port_conflict.lock().unwrap() = false;
    if operation_cancelled(stack) {
        return Err(model_swap_failure(
            stack,
            &cfg,
            &current,
            restore_previous,
            "cancelled",
        ));
    }
    if let Err(error) = spawn_llama(stack, &cfg, &dest) {
        return Err(model_swap_failure(
            stack,
            &cfg,
            &current,
            restore_previous,
            &error,
        ));
    }
    let wait_result = wait_llama_ready(
        stack,
        cfg.llama_port,
        Duration::from_secs(120),
        true,
        |elapsed_ms| {
            emit_model_progress(
                app,
                stack,
                operation_id,
                lang,
                &actual_quant,
                "loading",
                [("elapsedMs", json!(elapsed_ms))],
            );
        },
    );
    if let Err(error) = wait_result {
        let message = error.message();
        return Err(model_swap_failure(
            stack,
            &cfg,
            &current,
            restore_previous,
            &message,
        ));
    }
    if operation_cancelled(stack) {
        return Err(model_swap_failure(
            stack,
            &cfg,
            &current,
            restore_previous,
            "cancelled",
        ));
    }

    if let Err(error) = commit_model_selection(stack, &dest, lang, &actual_quant) {
        return Err(model_swap_failure(
            stack,
            &cfg,
            &current,
            restore_previous,
            &format!("the new voice started, but its selection could not be saved: {error}"),
        ));
    }
    let mut ready_fields = vec![("switched", json!(true))];
    if actual_quant != requested_quant {
        ready_fields.push(("fallbackFrom", json!(requested_quant)));
        ready_fields.push((
            "message",
            json!(format!(
                "{requested_quant} is unavailable; loaded {actual_quant}"
            )),
        ));
    }
    emit_model_progress(
        app,
        stack,
        operation_id,
        lang,
        &actual_quant,
        "ready",
        ready_fields,
    );
    Ok(ModelSwitchResult {
        operation_id: operation_id.to_string(),
        language: lang.to_string(),
        preferred_quant: actual_quant.clone(),
        loaded_quant: actual_quant,
        switched: true,
    })
}

fn try_model_lifecycle(stack: &Stack) -> Result<std::sync::MutexGuard<'_, ()>, String> {
    match stack.lifecycle.try_lock() {
        Ok(guard) => Ok(guard),
        Err(TryLockError::WouldBlock) => {
            Err("another speech-stack or model operation is already in progress".to_string())
        }
        Err(TryLockError::Poisoned(_)) => Err("speech-stack lifecycle lock failed".to_string()),
    }
}

// Switch the explicit language/quant pair as one correlated operation. This is
// the primary UI contract: validation and lifecycle rejection happen before any
// download or process mutation, and every post-operationId failure is emitted
// with the same identifier so the panel can retire exactly the matching row.
pub fn set_model_selection(
    app: &AppHandle,
    stack: &Stack,
    lang: &str,
    quant: &str,
    operation_id: &str,
) -> Result<ModelSwitchResult, String> {
    validate_operation_id(operation_id)?;
    if model_base_for_lang(lang).is_none() {
        return Err(reject_model_switch(
            app,
            stack,
            operation_id,
            lang,
            quant,
            format!("unsupported language: {lang}"),
        ));
    }
    if let Err(error) = validate_quant(quant) {
        return Err(reject_model_switch(
            app,
            stack,
            operation_id,
            lang,
            quant,
            error,
        ));
    }
    let _lifecycle = try_model_lifecycle(stack)
        .map_err(|error| reject_model_switch(app, stack, operation_id, lang, quant, error))?;
    if stack.lifecycle_interrupt.load(Ordering::Acquire) {
        return Err(reject_model_switch(
            app,
            stack,
            operation_id,
            lang,
            quant,
            "speech stack is restarting".to_string(),
        ));
    }
    execute_model_switch(app, stack, operation_id, lang, quant)
}

// Ensure `lang` is downloaded at the current preferred size and make it the
// actual live selection. Blocking; the Tauri command runs this off the UI thread.
pub fn set_language(
    app: &AppHandle,
    stack: &Stack,
    lang: &str,
    operation_id: &str,
) -> Result<ModelSwitchResult, String> {
    let quant = stack.current_quant();
    set_model_selection(app, stack, lang, &quant, operation_id)
}

// A size selection is an actual, consented switch: download it when necessary,
// load it, and only then persist the quant alongside the model and language.
pub fn set_quant(
    app: &AppHandle,
    stack: &Stack,
    quant: &str,
    operation_id: &str,
) -> Result<ModelSwitchResult, String> {
    let lang = stack.current_language().unwrap_or_else(|| {
        stack
            .cfg
            .lock()
            .unwrap()
            .as_ref()
            .and_then(|config| config.language.clone())
            .unwrap_or_else(|| "en".to_string())
    });
    set_model_selection(app, stack, &lang, quant, operation_id)
}

// Language codes whose model file is already downloaded.
#[tauri::command]
pub fn installed_languages(
    state: tauri::State<'_, Stack>,
    quant: Option<String>,
) -> Result<Vec<String>, String> {
    let quant = {
        let guard = state.cfg.lock().unwrap();
        quant
            .or_else(|| guard.as_ref().map(|c| c.quant.clone()))
            .unwrap_or_else(d_quant)
    };
    validate_quant(&quant)?;
    let dir = state.model_dir();
    Ok(["en", "fr", "de", "ko", "hi", "zh", "es", "it"]
        .iter()
        .filter(|l| {
            model_base_for_lang(l)
                .map(|b| dir.join(model_file(b, &quant)).is_file())
                .unwrap_or(false)
        })
        .map(|l| (*l).to_string())
        .collect())
}

#[tauri::command]
pub async fn stack_status(app: AppHandle) -> Result<serde_json::Value, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let state = app.state::<Stack>();
        stack_status_inner(state.inner())
    })
    .await
    .map_err(|error| error.to_string())
}

fn stack_status_inner(state: &Stack) -> serde_json::Value {
    let cfg = state.cfg.lock().unwrap().clone();
    let runtime = state.runtime.lock().unwrap().clone();
    let managed_paths = state.managed_paths.lock().unwrap().clone();
    let (llama_port, orpheus_port) = cfg
        .as_ref()
        .map(|c| (c.llama_port, c.orpheus_port))
        .unwrap_or((d_llama_port(), d_orpheus_port()));
    let config_ready = cfg.is_some();
    let (llama_ready, backend_ready) = std::thread::scope(|scope| {
        let llama_probe = scope.spawn(|| llama_ready(llama_port));
        let backend_probe = scope.spawn(|| backend_ready(orpheus_port));
        (
            llama_probe.join().unwrap_or(false),
            backend_probe.join().unwrap_or(false),
        )
    });
    let llama_port_conflict = !llama_ready && *state.llama_port_conflict.lock().unwrap();
    let backend_port_conflict = !backend_ready && *state.backend_port_conflict.lock().unwrap();
    let llama_reused = {
        let mut process = state.llama_process.lock().unwrap();
        let exited = process
            .child
            .as_mut()
            .is_some_and(|child| matches!(child.try_wait(), Ok(Some(_))));
        if exited {
            process.child.take();
        }
        process.reused || (llama_ready && process.child.is_none())
    };
    let port_conflict = llama_port_conflict || backend_port_conflict;
    let backend_present = runtime.as_ref().is_some_and(RuntimeInfo::backend_present);
    let runtime_present = runtime.as_ref().is_some_and(RuntimeInfo::runtime_present);
    let model_present = cfg
        .as_ref()
        .is_some_and(|config| Path::new(&config.model).is_file());
    let preferred_quant = cfg
        .as_ref()
        .map(|config| config.quant.clone())
        .unwrap_or_else(d_quant);
    let loaded_quant = cfg.as_ref().and_then(|config| {
        let model = Path::new(&config.model);
        model
            .is_file()
            .then(|| quant_for_model_path(model))
            .flatten()
    });
    let model_operation = {
        let operation = state.model_operation.lock().unwrap();
        json!({
            "active": operation.active,
            "operationId": operation.operation_id,
            "phase": operation.phase,
            "targetLanguage": operation.target_language,
            "targetQuant": operation.target_quant,
            "startedAtMs": operation.started_at_ms,
        })
    };
    let state_name = readiness_state(
        config_ready,
        runtime_present,
        backend_present,
        port_conflict,
        model_present,
        backend_ready,
        llama_ready,
    );
    let ready = state_name == "ready";

    let mut notes = state.notes.lock().unwrap().clone();
    let mut add_action = |message: &str| {
        if !notes.iter().any(|note| note == message) {
            notes.push(message.to_string());
        }
    };
    if !ready {
        if !runtime_present {
            add_action("Speech runtime missing or incomplete. Install or repair the runtime pack, then restart Orpheus Pet.");
        }
        if runtime_present && !backend_present {
            add_action("Speech backend files are missing or incomplete. Repair the runtime pack, then restart Orpheus Pet.");
        }
        if port_conflict {
            add_action("A speech-service port is occupied by an unhealthy or unrelated process. Close it, then retry.");
        }
        if !model_present {
            add_action("No voice model is installed. Choose a language to download its model.");
        }
        if runtime_present && !backend_ready {
            add_action(
                "Speech backend is not ready. Check orpheus-fastapi.log or restart Orpheus Pet.",
            );
        }
        if runtime_present && model_present && !llama_ready {
            add_action(
                "Voice inference is not ready. Check llama-server.log or reload the selected language.",
            );
        }
    }

    let runtime_status = runtime.as_ref().map(|info| {
        json!({
            "source": info.source,
            "manifestPath": info.manifest_path.as_ref().map(|path| path.to_string_lossy().to_string()),
            "version": info.version,
            "flavor": info.flavor,
            "decoderPresent": info.decoder_present(),
        })
    });
    let paths = json!({
        "config": state.cfg_path.lock().unwrap().as_ref().map(|path| path.to_string_lossy().to_string()),
        "models": state.model_dir().to_string_lossy(),
        "cache": managed_paths
            .as_ref()
            .map(|paths| paths.cache_dir.to_string_lossy().to_string()),
        "logs": managed_paths
            .as_ref()
            .map(|paths| paths.logs_dir.to_string_lossy().to_string())
            .or_else(|| cfg.as_ref().and_then(|config| config.logs_dir.clone())),
        "runtime": managed_paths
            .as_ref()
            .map(|paths| paths.runtime_dir.to_string_lossy().to_string()),
    });
    let managed_processes = {
        let mut processes = state.processes.lock().unwrap();
        processes.retain_mut(|process| !matches!(process.child.try_wait(), Ok(Some(_))));
        processes
            .iter()
            .map(|process| json!({ "name": process.name, "pid": process.child.id() }))
            .collect::<Vec<_>>()
    };
    json!({
        // Keep the original keys for older frontends while exposing the more
        // precise first-run readiness contract.
        "llamaUp": llama_ready,
        "orpheusUp": backend_ready,
        "configReady": config_ready,
        "runtimePresent": runtime_present,
        "backendPresent": backend_present,
        "portConflict": {
            "llama": llama_port_conflict,
            "backend": backend_port_conflict,
        },
        "ports": {
            "llama": llama_port,
            "backend": orpheus_port,
        },
        "modelPresent": model_present,
        "preferredQuant": preferred_quant,
        "loadedQuant": loaded_quant,
        "modelOperation": model_operation,
        "backendReady": backend_ready,
        "llamaReady": llama_ready,
        "llamaReused": llama_reused,
        "ready": ready,
        "state": state_name,
        "currentLanguage": state.current_language(),
        "runtime": runtime_status,
        "paths": paths,
        "managed": managed_processes,
        "notes": notes,
    })
}

fn readiness_state(
    config_ready: bool,
    runtime_present: bool,
    backend_present: bool,
    port_conflict: bool,
    model_present: bool,
    backend_ready: bool,
    llama_ready: bool,
) -> &'static str {
    // Preserve the long-standing reuse contract: if both real service
    // endpoints answer, the current session can speak even when an externally
    // managed stack has no matching local runtime/model files.
    if backend_ready && llama_ready {
        "ready"
    } else if !config_ready {
        "config-error"
    } else if !runtime_present {
        "runtime-missing"
    } else if !backend_present {
        "backend-missing"
    } else if port_conflict {
        "port-conflict"
    } else if !model_present {
        "model-missing"
    } else if !backend_ready {
        "starting-backend"
    } else if !llama_ready {
        "loading-model"
    } else {
        "ready"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_config(model: &str) -> StackConfig {
        StackConfig {
            llama_server: "llama/llama-server.exe".to_string(),
            model: model.to_string(),
            llama_port: d_llama_port(),
            llama_args: Vec::new(),
            orpheus_dir: "backend".to_string(),
            python: "backend/python.exe".to_string(),
            orpheus_port: d_orpheus_port(),
            logs_dir: Some("logs".to_string()),
            hotkey: d_hotkey(),
            language: Some("en".to_string()),
            quant: d_quant(),
        }
    }

    #[test]
    fn runtime_manifest_paths_are_relative_to_the_current_pack() {
        let manifest_path = Path::new("app-data/runtime/current/manifest.json");
        let runtime = runtime_from_manifest(
            manifest_path,
            RuntimeManifest {
                schema_version: RUNTIME_MANIFEST_SCHEMA,
                version: "2026.07.1".to_string(),
                flavor: Some("cpu".to_string()),
                llama_server: "llama/llama-server.exe".to_string(),
                backend_exe: "backend/orpheus-backend.exe".to_string(),
                backend_dir: None,
                backend_args: vec!["--quiet".to_string()],
            },
        )
        .unwrap();

        assert_eq!(
            runtime.llama_server,
            Path::new("app-data/runtime/current/llama/llama-server.exe")
        );
        assert_eq!(
            runtime.backend_exe.as_deref(),
            Some(Path::new(
                "app-data/runtime/current/backend/orpheus-backend.exe"
            ))
        );
        assert_eq!(
            runtime.backend_dir,
            Path::new("app-data/runtime/current/backend")
        );
    }

    #[test]
    fn runtime_manifest_rejects_parent_traversal() {
        let error =
            runtime_member(Path::new("runtime/current"), "../evil.exe", "backendExe").unwrap_err();
        assert!(error.contains("relative path"));
    }

    #[test]
    fn managed_paths_keep_models_local_and_logs_separate() {
        let managed = ManagedPaths {
            config_file: PathBuf::from("config/stack.config.json"),
            models_dir: PathBuf::from("local-data/models"),
            cache_dir: PathBuf::from("local-data/cache/huggingface"),
            logs_dir: PathBuf::from("local-data/logs"),
            runtime_dir: PathBuf::from("local-data/runtime"),
        };
        let mut cfg = test_config("legacy/models/Voice-Q8_0.gguf");

        apply_managed_paths(&mut cfg, &managed);

        assert_eq!(
            PathBuf::from(cfg.model),
            Path::new("local-data/models/Voice-Q8_0.gguf")
        );
        assert_eq!(cfg.logs_dir.as_deref(), Some("local-data/logs"));
    }

    #[test]
    fn managed_paths_supply_a_default_model_filename() {
        let managed = ManagedPaths {
            config_file: PathBuf::from("config/stack.config.json"),
            models_dir: PathBuf::from("local-data/models"),
            cache_dir: PathBuf::from("local-data/cache/huggingface"),
            logs_dir: PathBuf::from("local-data/logs"),
            runtime_dir: PathBuf::from("local-data/runtime"),
        };
        let mut cfg = test_config("");

        apply_managed_paths(&mut cfg, &managed);

        assert_eq!(
            PathBuf::from(cfg.model),
            Path::new("local-data/models/Orpheus-3b-FT-Q8_0.gguf")
        );
    }

    #[test]
    fn model_sizes_are_strictly_allowlisted() {
        for quant in SUPPORTED_QUANTS {
            assert!(validate_quant(quant).is_ok());
        }
        for quant in ["", "q8_0", "Q6_K", "../../outside", "Q8_0/../outside"] {
            assert!(validate_quant(quant).is_err(), "accepted {quant:?}");
        }
    }

    #[test]
    fn loaded_quant_comes_from_the_committed_model_filename() {
        assert_eq!(
            quant_for_model_path(Path::new("models/Orpheus-3b-FT-Q4_K_M.gguf")).as_deref(),
            Some("Q4_K_M")
        );
        assert_eq!(
            quant_for_model_path(Path::new("models/Orpheus-3b-FT-Q2_K.gguf")).as_deref(),
            Some("Q2_K")
        );
        assert_eq!(
            quant_for_model_path(Path::new("models/not-a-supported-model.gguf")),
            None
        );
    }

    #[test]
    fn overlapping_model_operations_are_rejected_and_guard_clears_state() {
        let stack = Stack::default();
        let first = ModelOperationGuard::begin(&stack, "first", "en", "Q8_0").unwrap();
        assert!(stack.model_operation.lock().unwrap().active);
        assert!(ModelOperationGuard::begin(&stack, "second", "de", "Q4_K_M").is_err());
        drop(first);
        assert!(!stack.model_operation.lock().unwrap().active);
    }

    #[test]
    fn downloaded_model_requires_size_and_gguf_magic() {
        let path = std::env::temp_dir().join(format!(
            "orpheus-gguf-validation-{}-{}.bin",
            std::process::id(),
            unix_time_ms()
        ));
        fs::write(&path, b"GGUFpayload").unwrap();
        assert!(validate_gguf_file(&path, 4).is_ok());
        assert!(validate_gguf_file(&path, 1_000).is_err());
        fs::write(&path, b"HTMLpayload").unwrap();
        assert!(validate_gguf_file(&path, 4).is_err());
        let _ = fs::remove_file(path);
    }

    #[test]
    fn readiness_reports_the_first_actionable_blocker() {
        assert_eq!(
            readiness_state(false, false, false, false, false, false, false),
            "config-error"
        );
        assert_eq!(
            readiness_state(true, false, false, false, false, false, false),
            "runtime-missing"
        );
        assert_eq!(
            readiness_state(true, true, false, false, false, false, false),
            "backend-missing"
        );
        assert_eq!(
            readiness_state(true, true, true, true, true, false, false),
            "port-conflict"
        );
        assert_eq!(
            readiness_state(true, true, true, false, false, true, false),
            "model-missing"
        );
        assert_eq!(
            readiness_state(true, true, true, false, true, true, false),
            "loading-model"
        );
        assert_eq!(
            readiness_state(true, true, true, false, true, true, true),
            "ready"
        );
        assert_eq!(
            readiness_state(false, false, false, false, false, true, true),
            "ready"
        );
    }
}
