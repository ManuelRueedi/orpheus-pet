// Manages the Orpheus voice stack as child processes so the pet is ONE program
// (llama-server + Orpheus-FastAPI with the /v1/audio/speech/stream endpoint):
// launching the pet brings up llama-server (GGUF inference) and Orpheus-FastAPI
// (token -> WAV via SNAC), and quitting the pet tears them down again.
//
// Behaviour:
//   - Paths/ports come from stack.config.json (searched next to the exe, in the
//     cwd, and one level up so `pnpm tauri dev` finds the repo copy; override
//     with the ORPHEUS_PET_CONFIG env var).
//   - If a port is already listening we DON'T spawn a duplicate — we reuse the
//     running server and, on quit, only kill what we spawned ourselves.
//   - Children run with hidden consoles; their output goes to logsDir.
use serde::Deserialize;
use serde_json::json;
use std::{
    fs,
    net::{SocketAddr, TcpStream},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::Mutex,
    time::{Duration, Instant},
};
use tauri::{AppHandle, Emitter};

#[cfg(windows)]
use std::os::windows::process::CommandExt;

#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

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

#[derive(Default)]
pub struct Stack {
    pids: Mutex<Vec<(String, u32)>>,
    notes: Mutex<Vec<String>>,
    cfg: Mutex<Option<StackConfig>>,
    cfg_path: Mutex<Option<PathBuf>>,
    hotkey_current: Mutex<Option<String>>,
    llama_pid: Mutex<Option<u32>>,
    language: Mutex<Option<String>>,
    download_pid: Mutex<Option<u32>>,
    download_cancel: Mutex<bool>,
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

    // Write the chosen hotkey back to stack.config.json so it survives restart.
    pub fn persist_hotkey(&self, combo: &str) -> Result<(), String> {
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

    // Write just the quant (size preference) back to stack.config.json.
    pub fn persist_quant(&self, quant: &str) -> Result<(), String> {
        let path = self
            .cfg_path
            .lock()
            .unwrap()
            .clone()
            .ok_or_else(|| "config path unknown".to_string())?;
        let text = fs::read_to_string(&path).map_err(|e| e.to_string())?;
        let mut v: serde_json::Value = serde_json::from_str(&text).map_err(|e| e.to_string())?;
        v["quant"] = serde_json::Value::String(quant.to_string());
        let out = serde_json::to_string_pretty(&v).map_err(|e| e.to_string())?;
        fs::write(&path, out).map_err(|e| e.to_string())?;
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

    fn set_model_path(&self, path: &Path) {
        if let Some(c) = self.cfg.lock().unwrap().as_mut() {
            c.model = path.to_string_lossy().to_string();
        }
    }

    // Persist the loaded model + language to stack.config.json, and mirror the
    // model filename into Orpheus-FastAPI's .env for tidiness.
    fn persist_model(&self, model_path: &Path, lang: &str) -> Result<(), String> {
        if let Some(path) = self.cfg_path.lock().unwrap().clone() {
            let text = fs::read_to_string(&path).map_err(|e| e.to_string())?;
            let mut v: serde_json::Value =
                serde_json::from_str(&text).map_err(|e| e.to_string())?;
            v["model"] = serde_json::Value::String(model_path.to_string_lossy().to_string());
            v["language"] = serde_json::Value::String(lang.to_string());
            let out = serde_json::to_string_pretty(&v).map_err(|e| e.to_string())?;
            fs::write(&path, out).map_err(|e| e.to_string())?;
        }
        let orpheus_dir = self.cfg.lock().unwrap().as_ref().map(|c| c.orpheus_dir.clone());
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
// where its relative paths ("../llama/…") resolve correctly. In a release build we
// fall back to next-to-the-exe. (Sidecar bundling will revisit the release side.)
fn default_config_target() -> Option<PathBuf> {
    if cfg!(debug_assertions) {
        std::env::current_dir()
            .ok()
            .and_then(|d| d.parent().map(|p| p.join("stack.config.json")))
    } else {
        std::env::current_exe()
            .ok()
            .and_then(|e| e.parent().map(|p| p.join("stack.config.json")))
    }
}

fn config_candidates() -> Vec<PathBuf> {
    let mut v = Vec::new();
    if let Ok(p) = std::env::var("ORPHEUS_PET_CONFIG") {
        v.push(PathBuf::from(p));
    }
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
    v
}

// Returns the parsed config, its path, and whether we just created it from the
// bundled default (true) vs. found an existing file (false).
fn load_config() -> Result<(StackConfig, PathBuf, bool), String> {
    let candidates = config_candidates();
    for path in &candidates {
        if path.is_file() {
            let text = fs::read_to_string(path)
                .map_err(|e| format!("failed to read {}: {e}", path.display()))?;
            let cfg: StackConfig = serde_json::from_str(&text)
                .map_err(|e| format!("failed to parse {}: {e}", path.display()))?;
            return Ok((cfg, path.clone(), false));
        }
    }
    // Nothing found: create stack.config.json from the baked-in example so a
    // fresh clone / first run works without the manual copy step. Parse first so
    // an invalid bundled default errors out instead of writing a broken file.
    if let Some(target) = default_config_target() {
        let cfg: StackConfig = serde_json::from_str(DEFAULT_CONFIG)
            .map_err(|e| format!("bundled default config is invalid: {e}"))?;
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

fn port_open(port: u16) -> bool {
    let addr: SocketAddr = ([127, 0, 0, 1], port).into();
    TcpStream::connect_timeout(&addr, Duration::from_millis(300)).is_ok()
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

fn spawn_named(stack: &Stack, name: &str, cmd: &mut Command, notes: &mut Vec<String>) {
    match cmd.spawn() {
        Ok(child) => {
            let pid = child.id();
            stack.pids.lock().unwrap().push((name.to_string(), pid));
            notes.push(format!("{name}: spawned (pid {pid})"));
        }
        Err(e) => notes.push(format!("{name}: FAILED to spawn: {e}")),
    }
}

fn kill_pid(pid: u32) {
    #[cfg(windows)]
    {
        let mut c = Command::new("taskkill");
        c.args(["/PID", &pid.to_string(), "/T", "/F"]);
        hidden(&mut c);
        let _ = c.status();
    }
    #[cfg(not(windows))]
    {
        let _ = Command::new("kill").args(["-9", &pid.to_string()]).status();
    }
}

fn kill_llama(stack: &Stack) {
    if let Some(pid) = stack.llama_pid.lock().unwrap().take() {
        kill_pid(pid);
    }
}

// PID LISTENING on a local TCP port (Windows netstat). Used to free the llama
// port even when we reused a server we didn't spawn (dev hot-reload).
fn pid_on_port(port: u16) -> Option<u32> {
    let out = Command::new("netstat").args(["-ano"]).output().ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    let suffix = format!(":{port}");
    for line in text.lines() {
        if !line.to_uppercase().contains("LISTENING") {
            continue;
        }
        let toks: Vec<&str> = line.split_whitespace().collect();
        if toks.len() >= 5 && toks[1].ends_with(&suffix) {
            if let Ok(pid) = toks[toks.len() - 1].parse::<u32>() {
                return Some(pid);
            }
        }
    }
    None
}

fn spawn_llama(stack: &Stack, cfg: &StackConfig, model_path: &Path) {
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
        Ok(child) => *stack.llama_pid.lock().unwrap() = Some(child.id()),
        Err(e) => note(stack, format!("llama-server spawn failed: {e}")),
    }
}

fn wait_port(port: u16, timeout: Duration) -> bool {
    let start = Instant::now();
    while start.elapsed() < timeout {
        if port_open(port) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(500));
    }
    false
}

pub fn start(stack: &Stack) {
    let mut notes = Vec::new();
    let (mut cfg, cfg_path, created) = match load_config() {
        Ok(v) => v,
        Err(e) => {
            stack
                .notes
                .lock()
                .unwrap()
                .push(format!("config error: {e} — start the servers manually"));
            return;
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
    *stack.cfg.lock().unwrap() = Some(cfg.clone());
    *stack.cfg_path.lock().unwrap() = Some(cfg_path);

    // 1. llama-server (GGUF model inference on the GPU)
    if port_open(cfg.llama_port) {
        notes.push(format!(
            "llama-server: already running on :{}, reusing",
            cfg.llama_port
        ));
    } else if Path::new(&cfg.model).is_file() {
        spawn_llama(stack, &cfg, Path::new(&cfg.model));
        notes.push("llama-server: spawned".to_string());
    } else {
        // Fresh clone with no model yet: don't spawn a doomed llama-server.
        // Picking a language in the panel downloads a model and starts it.
        notes.push(format!(
            "llama-server: no model at {} — pick a language in the panel to download one",
            cfg.model
        ));
    }

    // Current UI language: persisted choice, else derived from the loaded model.
    let init_lang = cfg
        .language
        .clone()
        .unwrap_or_else(|| lang_for_model_file(&cfg.model).to_string());
    stack.set_current_language(&init_lang);

    // 2. Orpheus-FastAPI (uvicorn without --reload: single process, clean kill)
    enforce_orpheus_env(&cfg, &mut notes);
    if port_open(cfg.orpheus_port) {
        notes.push(format!(
            "orpheus-fastapi: already running on :{}, reusing",
            cfg.orpheus_port
        ));
    } else {
        let (out, err) = stdio_logs(&cfg.logs_dir, "orpheus-fastapi.log");
        let mut cmd = Command::new(&cfg.python);
        cmd.args([
            "-m",
            "uvicorn",
            "app:app",
            "--host",
            "127.0.0.1",
            "--port",
            &cfg.orpheus_port.to_string(),
        ])
        .current_dir(&cfg.orpheus_dir)
        .env("PYTHONUTF8", "1") // its emoji log lines crash on cp1252 otherwise
        .stdin(Stdio::null())
        .stdout(out)
        .stderr(err);
        hidden(&mut cmd);
        spawn_named(stack, "orpheus-fastapi", &mut cmd, &mut notes);
    }

    stack.notes.lock().unwrap().extend(notes);
}

// Kill everything we spawned (whole process trees). Servers we merely reused
// are left alone.
pub fn stop_all(stack: &Stack) {
    kill_llama(stack);
    let pids: Vec<(String, u32)> = stack.pids.lock().unwrap().drain(..).collect();
    for (_name, pid) in pids {
        #[cfg(windows)]
        {
            let mut c = Command::new("taskkill");
            c.args(["/PID", &pid.to_string(), "/T", "/F"]);
            hidden(&mut c);
            let _ = c.status();
        }
        #[cfg(not(windows))]
        {
            let _ = Command::new("kill").args(["-9", &pid.to_string()]).status();
        }
    }
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

fn curl_content_length(url: &str) -> Option<u64> {
    let out = Command::new("curl").args(["-sIL", url]).output().ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    let mut last = None;
    for line in text.lines() {
        let l = line.trim().to_ascii_lowercase();
        if let Some(v) = l.strip_prefix("content-length:") {
            if let Ok(n) = v.trim().parse::<u64>() {
                last = Some(n);
            }
        }
    }
    last
}

// Download to a .part file, emitting "model-progress" as it grows, then rename.
// Cancellable via cancel_download (kills the curl child; returns Err "cancelled").
fn download_model(
    app: &AppHandle,
    stack: &Stack,
    lang: &str,
    url: &str,
    dest: &Path,
) -> Result<(), String> {
    let total = curl_content_length(url);
    let part = dest.with_extension("part");
    let _ = fs::remove_file(&part);
    *stack.download_cancel.lock().unwrap() = false;

    let mut cmd = Command::new("curl");
    cmd.args(["-L", "--fail", "-o"])
        .arg(&part)
        .arg(url)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    hidden(&mut cmd);
    let mut child = cmd.spawn().map_err(|e| format!("curl spawn failed: {e}"))?;
    *stack.download_pid.lock().unwrap() = Some(child.id());

    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                *stack.download_pid.lock().unwrap() = None;
                if status.success() {
                    fs::rename(&part, dest).map_err(|e| format!("rename failed: {e}"))?;
                    let _ = app.emit(
                        "model-progress",
                        json!({"lang": lang, "phase": "download", "pct": 100u64, "received": total, "total": total}),
                    );
                    return Ok(());
                }
                let _ = fs::remove_file(&part);
                if *stack.download_cancel.lock().unwrap() {
                    return Err("cancelled".to_string());
                }
                return Err(format!("download failed (curl exit {:?})", status.code()));
            }
            Ok(None) => {
                let received = fs::metadata(&part).map(|m| m.len()).unwrap_or(0);
                let pct = total.map(|t| if t > 0 { received * 100 / t } else { 0 });
                let _ = app.emit(
                    "model-progress",
                    json!({"lang": lang, "phase": "download", "pct": pct, "received": received, "total": total}),
                );
                std::thread::sleep(Duration::from_millis(600));
            }
            Err(e) => return Err(format!("curl wait error: {e}")),
        }
    }
}

// Cancel an in-flight model download (kills the curl child).
pub fn cancel_download(stack: &Stack) {
    *stack.download_cancel.lock().unwrap() = true;
    if let Some(pid) = stack.download_pid.lock().unwrap().take() {
        kill_pid(pid);
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
) -> serde_json::Value {
    let (dir, quant) = {
        let guard = state.cfg.lock().unwrap();
        let dir = guard
            .as_ref()
            .and_then(|c| Path::new(&c.model).parent().map(|p| p.to_path_buf()))
            .unwrap_or_else(|| PathBuf::from("models"));
        let quant = quant
            .or_else(|| guard.as_ref().map(|c| c.quant.clone()))
            .unwrap_or_else(d_quant);
        (dir, quant)
    };
    match model_base_for_lang(&lang) {
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
    }
}

// Ensure the model for `lang` is present (download if needed) and loaded by
// llama-server, then persist the choice. Blocking; run off the UI thread.
pub fn set_language(app: &AppHandle, stack: &Stack, lang: &str) -> Result<(), String> {
    let cfg = stack
        .cfg
        .lock()
        .unwrap()
        .clone()
        .ok_or_else(|| "stack not started".to_string())?;
    let base = model_base_for_lang(lang).ok_or_else(|| format!("unsupported language: {lang}"))?;
    let dir = Path::new(&cfg.model)
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| PathBuf::from("models"));

    // Preferred quant from config; not every language publishes every quant, so
    // fall back to Q8_0 (always available) if the chosen one can't be fetched.
    let quant = cfg.quant.clone();
    let mut file = model_file(base, &quant);
    let mut dest = dir.join(&file);
    let current = PathBuf::from(&cfg.model);

    // Already the loaded model (Spanish & Italian share one)? Just switch the
    // UI language — no download, no reload.
    if dest == current && dest.is_file() && port_open(cfg.llama_port) {
        stack.set_current_language(lang);
        let _ = stack.persist_model(&dest, lang);
        let _ = app.emit("model-progress", json!({"lang": lang, "phase": "ready"}));
        return Ok(());
    }

    if !dest.is_file() {
        if quant != "Q8_0" && curl_content_length(&hf_url(&file)).is_none() {
            note(stack, format!("{quant} unavailable for {lang}; using Q8_0"));
            file = model_file(base, "Q8_0");
            dest = dir.join(&file);
        }
        if !dest.is_file() {
            let _ =
                app.emit("model-progress", json!({"lang": lang, "phase": "download", "pct": 0u64}));
            download_model(app, stack, lang, &hf_url(&file), &dest)?;
        }
    }

    // Swap llama-server to the new model.
    let _ = app.emit("model-progress", json!({"lang": lang, "phase": "loading"}));
    kill_llama(stack);
    if let Some(pid) = pid_on_port(cfg.llama_port) {
        kill_pid(pid);
    }
    let freed = Instant::now();
    while port_open(cfg.llama_port) && freed.elapsed() < Duration::from_secs(10) {
        std::thread::sleep(Duration::from_millis(200));
    }
    spawn_llama(stack, &cfg, &dest);
    if !wait_port(cfg.llama_port, Duration::from_secs(120)) {
        return Err("model ready, but llama-server did not come up".to_string());
    }

    stack.set_model_path(&dest);
    stack.set_current_language(lang);
    let _ = stack.persist_model(&dest, lang);
    let _ = app.emit("model-progress", json!({"lang": lang, "phase": "ready"}));
    Ok(())
}

// Change the quantisation (voice-model size) and reload the CURRENT language's
// model at the new quant: download if needed, then hot-swap llama-server. This is
// the panel's "model size / performance" knob — smaller quants keep the pet usable
// on low-spec PCs. Persists to stack.config.json so the choice sticks.
// Set the preferred voice-model size (quant) — the panel's "detail" knob. This is
// a DOWNLOAD PREFERENCE, deliberately decoupled from the loaded voice:
//   - always record the size (in-memory + persisted) so the size dropdown, the
//     per-language download sizes, and the NEXT language download all use it;
//   - if the current language already has a model at this size on disk, hot-swap
//     to it so the playing voice matches the selected detail;
//   - otherwise DON'T download — the current voice keeps playing and the new size
//     takes effect the next time a language is picked.
pub fn set_quant(app: &AppHandle, stack: &Stack, quant: &str) -> Result<(), String> {
    let prev = stack.current_quant();
    {
        let mut guard = stack.cfg.lock().unwrap();
        match guard.as_mut() {
            Some(c) => c.quant = quant.to_string(),
            None => return Err("stack not started".to_string()),
        }
    }
    let _ = stack.persist_quant(quant);
    if quant == prev {
        return Ok(());
    }
    // Hot-swap only if the model at this size is already downloaded — never fetch
    // here (that's what picking a language does).
    let cfg = stack
        .cfg
        .lock()
        .unwrap()
        .clone()
        .ok_or_else(|| "stack not started".to_string())?;
    let lang = stack.current_language().unwrap_or_else(|| "en".to_string());
    if let Some(base) = model_base_for_lang(&lang) {
        let dir = Path::new(&cfg.model)
            .parent()
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| PathBuf::from("models"));
        if dir.join(model_file(base, quant)).is_file() {
            return set_language(app, stack, &lang);
        }
    }
    Ok(())
}

// Language codes whose model file is already downloaded.
#[tauri::command]
pub fn installed_languages(state: tauri::State<'_, Stack>) -> Vec<String> {
    let (dir, quant) = {
        let guard = state.cfg.lock().unwrap();
        let dir = guard
            .as_ref()
            .and_then(|c| Path::new(&c.model).parent().map(|p| p.to_path_buf()))
            .unwrap_or_else(|| PathBuf::from("models"));
        let quant = guard.as_ref().map(|c| c.quant.clone()).unwrap_or_else(d_quant);
        (dir, quant)
    };
    ["en", "fr", "de", "ko", "hi", "zh", "es", "it"]
        .iter()
        .filter(|l| {
            model_base_for_lang(l)
                .map(|b| dir.join(model_file(b, &quant)).is_file())
                .unwrap_or(false)
        })
        .map(|l| (*l).to_string())
        .collect()
}

#[tauri::command]
pub fn stack_status(state: tauri::State<'_, Stack>) -> serde_json::Value {
    let (llama_port, orpheus_port) = state
        .cfg
        .lock()
        .unwrap()
        .as_ref()
        .map(|c| (c.llama_port, c.orpheus_port))
        .unwrap_or((d_llama_port(), d_orpheus_port()));
    json!({
        "llamaUp": port_open(llama_port),
        "orpheusUp": port_open(orpheus_port),
        "managed": state
            .pids
            .lock()
            .unwrap()
            .iter()
            .map(|(n, p)| json!({ "name": n, "pid": p }))
            .collect::<Vec<_>>(),
        "notes": state.notes.lock().unwrap().clone(),
    })
}
