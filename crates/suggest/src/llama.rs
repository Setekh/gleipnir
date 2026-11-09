//! A small code model, run on this machine by llama.cpp's server, for suggesting the rest of a
//! line from the code before and after the cursor. llama.cpp and the model are installed the first
//! time they're needed, pinned and checksummed like every tool, into Gleipnir's tools folder. The
//! server listens on 127.0.0.1 only, is shared by every window, and stops when Gleipnir does.
//!
//! A suggestion's score is the model's own probability of that exact text, from the odds it gave
//! each of its tokens.

use anyhow::{Context as _, Result, bail};
use gleipnir_packages::install::{InstallContext, install_tool};
use gleipnir_packages::manifest::{ArchiveFormat, Asset, InstallSpec, ToolSpec};
use serde_json::{Value, json};
use std::io::{Read as _, Write as _};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

/// The llama.cpp build used, released 2026-10-25.
const LLAMA_VERSION: &str = "b11889";

/// A model to suggest with: fill-in-the-middle code models in llama.cpp's own format.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ModelChoice {
    pub name: &'static str,
    file: &'static str,
    url: &'static str,
    sha256: &'static str,
    pub size: u64,
}

/// The models offered, smallest first. Qwen2.5-Coder, Apache-2.0, as converted by ggml-org.
pub const MODELS: [ModelChoice; 2] = [
    ModelChoice {
        name: "Qwen2.5-Coder 0.5B",
        file: "qwen2.5-coder-0.5b-q8_0.gguf",
        url: "https://huggingface.co/ggml-org/Qwen2.5-Coder-0.5B-Q8_0-GGUF/resolve/main/qwen2.5-coder-0.5b-q8_0.gguf",
        sha256: "d0f8cd6c49bab52a0abdbe47948518b1f4d9b1a8a2a6825099cea31cea10ac56",
        size: 531_068_128,
    },
    ModelChoice {
        name: "Qwen2.5-Coder 1.5B",
        file: "qwen2.5-coder-1.5b-q8_0.gguf",
        url: "https://huggingface.co/ggml-org/Qwen2.5-Coder-1.5B-Q8_0-GGUF/resolve/main/qwen2.5-coder-1.5b-q8_0.gguf",
        sha256: "29871c94d15727a6e243f79a37113d4ae625a6215b5e800bf41a23af2da32832",
        size: 1_646_573_056,
    },
];

pub fn model_named(name: &str) -> ModelChoice {
    MODELS
        .iter()
        .copied()
        .find(|m| m.name == name)
        .unwrap_or(MODELS[0])
}

/// llama.cpp's server, as a tool to install.
fn server_spec() -> ToolSpec {
    let asset = |platform: &str, file: &str, sha256: &str| Asset {
        platform: platform.into(),
        url: format!(
            "https://github.com/ggml-org/llama.cpp/releases/download/{LLAMA_VERSION}/{file}"
        ),
        sha256: sha256.into(),
        format: ArchiveFormat::TarGz,
    };
    ToolSpec {
        binary: "llama-server".into(),
        project_paths: Vec::new(),
        sdk_paths: Vec::new(),
        system_paths: Vec::new(),
        install: Some(InstallSpec::Download {
            version: LLAMA_VERSION.into(),
            binary: format!("llama-{LLAMA_VERSION}/llama-server"),
            assets: vec![
                asset(
                    "macos-aarch64",
                    "llama-b11889-bin-macos-arm64.tar.gz",
                    "4a3aa4a1e6425d6e0709dc3752943f5e58f4fde6ccb9cebdf0f5de8d1d694f57",
                ),
                asset(
                    "macos-x86_64",
                    "llama-b11889-bin-macos-x64.tar.gz",
                    "96776cca2f52f19808af2ec7f3ea2a9f8ac1e5e4d3404ca0238d75429602f3f5",
                ),
                asset(
                    "linux-x86_64",
                    "llama-b11889-bin-ubuntu-x64.tar.gz",
                    "2076290f3b80b7642e2dc165c01fbf1be386a56b4612ba45576d20527834d350",
                ),
                asset(
                    "linux-aarch64",
                    "llama-b11889-bin-ubuntu-arm64.tar.gz",
                    "893adf31c545272dde71df080294f0b7dc27fd182a2e5eda51088cdc2179d68d",
                ),
            ],
        }),
    }
}

/// Where things stand with the local model.
#[derive(Clone, Debug, PartialEq)]
pub enum Status {
    /// Not asked for yet, or turned off.
    Idle,
    /// Downloading: what, and how far, from 0 to 1.
    Downloading(String, f32),
    Starting,
    Ready,
    Failed(String),
}

struct Server {
    model: &'static str,
    port: u16,
    child: Child,
}

struct State {
    status: Status,
    server: Option<Server>,
    /// Whether setup is running, so it isn't started twice.
    busy: bool,
}

fn state() -> &'static Mutex<State> {
    static STATE: OnceLock<Mutex<State>> = OnceLock::new();
    STATE.get_or_init(|| {
        Mutex::new(State {
            status: Status::Idle,
            server: None,
            busy: false,
        })
    })
}

pub fn status() -> Status {
    state()
        .lock()
        .map(|s| s.status.clone())
        .unwrap_or(Status::Idle)
}

fn set_status(status: Status) {
    if let Ok(mut state) = state().lock() {
        state.status = status;
    }
}

/// The port of a server running `model`, or none yet, after starting it in the background if
/// `install` allows fetching what's missing, or it's all there already.
pub fn ensure(model: ModelChoice, install: bool) -> Option<u16> {
    let mut guard = state().lock().ok()?;
    if let Some(server) = &guard.server {
        if server.model == model.name {
            return Some(server.port);
        }
        // Another model was chosen: stop this one and start that.
        let mut old = guard.server.take()?;
        old.child.kill().ok();
    }
    if guard.busy || matches!(guard.status, Status::Failed(_)) {
        return None;
    }
    let tools = gleipnir_packages::managed_tools_dir();
    if !install && !installed(&tools, model) {
        return None;
    }
    guard.busy = true;
    drop(guard);
    std::thread::spawn(move || {
        let result = start(&tools, model);
        let mut state = state().lock().expect("the model's state");
        state.busy = false;
        match result {
            Ok(server) => {
                state.status = Status::Ready;
                state.server = Some(server);
            }
            Err(error) => state.status = Status::Failed(format!("{error:#}")),
        }
    });
    None
}

/// Lets a failed setup be tried again, as after the setting is turned off and on.
pub fn reset() {
    if let Ok(mut state) = state().lock()
        && matches!(state.status, Status::Failed(_))
    {
        state.status = Status::Idle;
    }
}

/// Stops the server, as when Gleipnir quits.
pub fn shutdown() {
    if let Ok(mut state) = state().lock()
        && let Some(mut server) = state.server.take()
    {
        server.child.kill().ok();
        server.child.wait().ok();
        SERVER_PID.store(0, std::sync::atomic::Ordering::SeqCst);
        std::fs::remove_file(pid_file()).ok();
    }
}

fn model_path(tools: &Path, model: ModelChoice) -> PathBuf {
    tools.join("models").join(model.file)
}

fn installed(tools: &Path, model: ModelChoice) -> bool {
    let server = gleipnir_packages::tools::managed_binary("llama.cpp", &server_spec(), tools);
    server.is_some_and(|s| s.is_file()) && model_path(tools, model).is_file()
}

/// Where the running server's process id is kept, so one left behind by a crash is stopped the
/// next time.
fn pid_file() -> PathBuf {
    gleipnir_packages::state_dir().join("llama-server.pid")
}

/// Stops a server an earlier run left behind, if it's still ours: running llama.cpp's server.
fn stop_leftover() {
    let Ok(pid) = std::fs::read_to_string(pid_file()) else {
        return;
    };
    std::fs::remove_file(pid_file()).ok();
    let Ok(pid) = pid.trim().parse::<i32>() else {
        return;
    };
    let ours = Command::new("ps")
        .args(["-p", &pid.to_string(), "-o", "command="])
        .output()
        .is_ok_and(|o| String::from_utf8_lossy(&o.stdout).contains("llama-server"));
    if ours {
        // SAFETY: sending a signal to a process id has no memory effects.
        unsafe {
            libc::kill(pid, libc::SIGTERM);
        }
    }
}

/// The server's process id, for stopping it however Gleipnir ends.
static SERVER_PID: std::sync::atomic::AtomicI32 = std::sync::atomic::AtomicI32::new(0);

extern "C" fn stop_at_exit() {
    let pid = SERVER_PID.load(std::sync::atomic::Ordering::SeqCst);
    if pid > 0 {
        // SAFETY: as above.
        unsafe {
            libc::kill(pid, libc::SIGTERM);
        }
        std::fs::remove_file(pid_file()).ok();
    }
}

fn start(tools: &Path, model: ModelChoice) -> Result<Server> {
    static AT_EXIT: std::sync::Once = std::sync::Once::new();
    AT_EXIT.call_once(|| {
        stop_leftover();
        // SAFETY: registering a plain function to run at exit.
        unsafe {
            libc::atexit(stop_at_exit);
        }
    });
    set_status(Status::Downloading("llama.cpp".into(), 0.));
    let env = gleipnir_packages::env::user_env();
    let binary = install_tool(
        "llama.cpp",
        &server_spec(),
        &InstallContext {
            managed_dir: tools,
            env: &env,
            rust_toolchain: None,
        },
    )?;
    let weights = model_path(tools, model);
    if !weights.is_file() {
        download(model, &weights)?;
    }
    set_status(Status::Starting);
    let port = std::net::TcpListener::bind("127.0.0.1:0")?
        .local_addr()?
        .port();
    let mut child = Command::new(&binary)
        .args(["-m"])
        .arg(&weights)
        .args(["--host", "127.0.0.1", "--port", &port.to_string()])
        // On the GPU where there is one; a context for a few thousand characters each side; and
        // the prompt's cache reused as typing moves on, which makes the next ask quick.
        .args([
            "-ngl",
            "99",
            "-c",
            "4096",
            "-ub",
            "1024",
            "-b",
            "1024",
            "--cache-reuse",
            "256",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .context("couldn't start llama.cpp's server")?;
    SERVER_PID.store(child.id() as i32, std::sync::atomic::Ordering::SeqCst);
    std::fs::create_dir_all(gleipnir_packages::state_dir()).ok();
    std::fs::write(pid_file(), child.id().to_string()).ok();
    let started = Instant::now();
    loop {
        if let Some(exit) = child.try_wait()? {
            bail!("llama.cpp's server stopped as it started ({exit})");
        }
        let healthy = ureq::get(&format!("http://127.0.0.1:{port}/health"))
            .call()
            .ok()
            .and_then(|mut r| r.body_mut().read_to_string().ok())
            .is_some_and(|body| body.contains("ok"));
        if healthy {
            break;
        }
        if started.elapsed() > Duration::from_secs(60) {
            child.kill().ok();
            bail!("llama.cpp's server didn't start within a minute");
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    Ok(Server {
        model: model.name,
        port,
        child,
    })
}

/// Downloads a model to `to`, checking its sha256 as it comes, through a file aside so a broken
/// download never looks like a model.
fn download(model: ModelChoice, to: &Path) -> Result<()> {
    use sha2::{Digest, Sha256};
    std::fs::create_dir_all(to.parent().context("no models folder")?)?;
    let partial = to.with_extension("partial");
    let mut response = ureq::get(model.url).call()?;
    let mut body = response.body_mut().with_config().limit(u64::MAX).reader();
    let mut file = std::fs::File::create(&partial)?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; 1 << 20];
    let mut done = 0u64;
    loop {
        let read = body.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
        file.write_all(&buffer[..read])?;
        done += read as u64;
        set_status(Status::Downloading(
            model.name.to_string(),
            (done as f32 / model.size as f32).min(1.),
        ));
    }
    file.flush()?;
    let actual: String = hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    if actual != model.sha256 {
        std::fs::remove_file(&partial).ok();
        bail!(
            "{} didn't match its checksum, so it wasn't kept",
            model.name
        );
    }
    std::fs::rename(&partial, to)?;
    Ok(())
}

/// What the model expects around the cursor: up to this much before and after, in bytes.
const BEFORE: usize = 3000;
const AFTER: usize = 1500;

/// The rest of the line at the cursor, as the model would write it, with the probability of
/// that exact text.
pub fn complete(port: u16, text: &str, offset: usize) -> Result<Option<(String, f32)>> {
    let floor = |mut at: usize| {
        while !text.is_char_boundary(at) {
            at -= 1;
        }
        at
    };
    let start = floor(offset.saturating_sub(BEFORE));
    let end = floor((offset + AFTER).min(text.len()));
    let body = json!({
        "input_prefix": &text[start..offset],
        "input_suffix": &text[offset..end],
        "prompt": "",
        "n_predict": 48,
        "temperature": 0,
        "top_k": 40,
        "samplers": ["top_k", "top_p", "infill"],
        "n_probs": 1,
        // One line, as a suggestion is the rest of the line.
        "stop": ["\n"],
        "cache_prompt": true,
    });
    let reply: Value = ureq::post(&format!("http://127.0.0.1:{port}/infill"))
        .config()
        .timeout_global(Some(Duration::from_secs(3)))
        .build()
        .send_json(&body)?
        .body_mut()
        .read_json()?;
    Ok(parse_completion(&reply))
}

/// The suggestion in a reply, and its probability: the product of its tokens' odds.
fn parse_completion(reply: &Value) -> Option<(String, f32)> {
    let content = reply["content"].as_str()?.trim_end().to_string();
    if content.trim().is_empty() {
        return None;
    }
    let logprob: f64 = reply["completion_probabilities"]
        .as_array()
        .map(|tokens| {
            tokens
                .iter()
                .filter_map(|t| t["logprob"].as_f64())
                .filter(|l| l.is_finite())
                .sum()
        })
        .unwrap_or(f64::NEG_INFINITY);
    Some((content, logprob.exp() as f32))
}

/// Whether the error means the server's gone, so it should be started again.
pub fn server_gone() {
    if let Ok(mut state) = state().lock()
        && let Some(server) = &mut state.server
        && server.child.try_wait().ok().flatten().is_some()
    {
        state.server = None;
        state.status = Status::Idle;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_reply_becomes_the_line_and_the_odds_of_all_of_it() {
        let reply = json!({
            "content": "Address),",
            "completion_probabilities": [
                { "token": "Address", "logprob": -0.1 },
                { "token": "),", "logprob": -0.2 },
            ],
        });
        let (text, score) = parse_completion(&reply).unwrap();
        assert_eq!(text, "Address),");
        assert!((score - (-0.3f32).exp()).abs() < 1e-6, "{score}");
        assert_eq!(parse_completion(&json!({ "content": "  " })), None);
    }

    #[test]
    fn models_are_found_by_name_with_the_smallest_by_default() {
        assert_eq!(
            model_named("Qwen2.5-Coder 1.5B").file,
            "qwen2.5-coder-1.5b-q8_0.gguf"
        );
        assert_eq!(model_named("gone").name, "Qwen2.5-Coder 0.5B");
        assert!(!MODELS[0].sha256.is_empty() && MODELS[0].size > 0);
    }

    /// Against the real thing, where `GLEIPNIR_LLAMA_TOOLS` names a tools folder that has it.
    #[test]
    fn the_real_model_suggests_the_rest_of_a_line() {
        let Some(tools) = std::env::var_os("GLEIPNIR_LLAMA_TOOLS").map(PathBuf::from) else {
            eprintln!("skipped: set GLEIPNIR_LLAMA_TOOLS to a tools folder with llama.cpp in it");
            return;
        };
        let mut server = start(&tools, MODELS[0]).unwrap();
        let text = "const fields = {\n  name: types.maybe(types.string),\n  address: types.maybe(ProvisioningAddress),\n  address2: types.maybe(Provisioning\n  city: types.maybe(types.string),\n}\n";
        let offset = text.find("(Provisioning\n").unwrap() + "(Provisioning".len();
        let mut found = None;
        for _ in 0..2 {
            found = complete(server.port, text, offset).unwrap();
        }
        server.child.kill().ok();
        let (line, score) = found.expect("a suggestion");
        assert!(line.starts_with("Address"), "{line}");
        assert!(score > 0.0 && score <= 1.0, "{score}");
    }
}
