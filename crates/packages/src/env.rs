//! Environment variables for the processes we start.
//!
//! An app opened from the Dock or a desktop launcher gets a minimal `PATH`, without anything a
//! shell profile adds: Homebrew, nvm, `~/.cargo/bin`. So, like other editors, we ask the user's
//! login shell for its environment once at startup.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Env {
    vars: BTreeMap<String, String>,
}

impl Env {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn from_process() -> Self {
        Self {
            vars: std::env::vars().collect(),
        }
    }

    pub fn from_pairs<'a>(pairs: impl IntoIterator<Item = (&'a str, &'a str)>) -> Self {
        Self {
            vars: pairs
                .into_iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
        }
    }

    pub fn get(&self, key: &str) -> Option<&str> {
        self.vars.get(key).map(String::as_str)
    }

    pub fn set(&mut self, key: impl Into<String>, value: impl Into<String>) {
        self.vars.insert(key.into(), value.into());
    }

    pub fn vars(&self) -> &BTreeMap<String, String> {
        &self.vars
    }

    /// The absolute directories on `PATH`, in order. Relative entries like `./node_modules/.bin`
    /// would depend on the working directory, so they're skipped.
    pub fn path_dirs(&self) -> Vec<PathBuf> {
        self.get("PATH")
            .map(|path| {
                std::env::split_paths(path)
                    .filter(|p| p.is_absolute())
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Finds an executable on `PATH`.
    pub fn which(&self, binary: &str) -> Option<PathBuf> {
        self.path_dirs()
            .into_iter()
            .map(|dir| dir.join(binary))
            .find(|p| is_executable(p))
    }

    /// Overlays another environment's variables on this one.
    pub fn merged_with(&self, other: &Env) -> Env {
        let mut vars = self.vars.clone();
        vars.extend(other.vars.clone());
        Env { vars }
    }

    /// Sets this environment on a command, replacing the inherited one.
    pub fn apply(&self, command: &mut Command) {
        command.env_clear();
        command.envs(&self.vars);
    }
}

pub fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::metadata(path).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

const MARKER_START: &str = "__GLEIPNIR_ENV_START__";
const MARKER_END: &str = "__GLEIPNIR_ENV_END__";

/// Asks a login shell for its environment. Interactive, because tools like nvm set up `PATH` in
/// `.zshrc` rather than a profile. Shell startup files can print anything, so the environment is
/// read from between markers, and a profile that hangs is given up on after `timeout`.
pub fn capture_login_shell(shell: &Path, timeout: Duration) -> Option<Env> {
    let script = format!("printf '{MARKER_START}'; env -0; printf '{MARKER_END}'");
    let mut child = Command::new(shell)
        .args(["-l", "-i", "-c", &script])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let started = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if started.elapsed() > timeout => {
                child.kill().ok();
                child.wait().ok();
                return None;
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(10)),
            Err(_) => return None,
        }
    }
    let output = child.wait_with_output().ok()?;
    parse_env_output(&String::from_utf8_lossy(&output.stdout))
}

fn parse_env_output(output: &str) -> Option<Env> {
    let start = output.find(MARKER_START)? + MARKER_START.len();
    let end = output[start..].find(MARKER_END)? + start;
    let vars = output[start..end]
        .split('\0')
        .filter_map(|entry| entry.split_once('='))
        .filter(|(key, _)| !key.is_empty() && !key.contains('\n'))
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
    Some(Env { vars })
}

static SHELL_ENV: OnceLock<Env> = OnceLock::new();

/// The environment to start tools with: the process's own, overlaid with the login shell's.
/// The first call does the capture, which takes about a second, so call `warm_up` at startup.
pub fn user_env() -> Env {
    SHELL_ENV
        .get_or_init(|| {
            let process = Env::from_process();
            // Started from a terminal, we already have the shell's environment.
            if std::env::var_os("TERM_PROGRAM").is_some()
                || std::env::var_os("GLEIPNIR_NO_SHELL_ENV").is_some()
            {
                return process;
            }
            let shell = process.get("SHELL").unwrap_or("/bin/zsh").to_string();
            match capture_login_shell(Path::new(&shell), Duration::from_secs(5)) {
                Some(shell_env) => process.merged_with(&shell_env),
                None => process,
            }
        })
        .clone()
}

/// Starts capturing the user's environment on a background thread.
pub fn warm_up() {
    std::thread::spawn(|| {
        user_env();
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn output_between_markers_is_parsed_and_noise_around_it_ignored() {
        let output =
            format!("Welcome!\n{MARKER_START}PATH=/a:/b\0HOME=/h\0EMPTY=\0{MARKER_END}bye");
        let env = parse_env_output(&output).unwrap();
        assert_eq!(env.get("PATH"), Some("/a:/b"));
        assert_eq!(env.get("HOME"), Some("/h"));
        assert_eq!(env.get("EMPTY"), Some(""));
    }

    #[test]
    fn values_with_newlines_and_equals_signs_survive() {
        let output = format!("{MARKER_START}A=x=y\nz\0{MARKER_END}");
        assert_eq!(parse_env_output(&output).unwrap().get("A"), Some("x=y\nz"));
    }

    #[test]
    fn missing_markers_mean_no_environment() {
        assert!(parse_env_output("nothing here").is_none());
    }

    #[test]
    fn relative_path_entries_are_skipped() {
        let env = Env::from_pairs([("PATH", "./node_modules/.bin:/usr/bin:relative")]);
        assert_eq!(env.path_dirs(), [PathBuf::from("/usr/bin")]);
    }

    #[test]
    fn which_finds_only_executables() {
        let dir = tempfile::tempdir().unwrap();
        let tool = dir.path().join("tool");
        std::fs::write(&tool, "#!/bin/sh\n").unwrap();
        let env = Env::from_pairs([("PATH", dir.path().to_str().unwrap())]);
        assert!(env.which("tool").is_none(), "not executable yet");
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&tool, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(env.which("tool"), Some(tool));
    }

    #[test]
    fn a_real_login_shell_can_be_captured() {
        let env = capture_login_shell(Path::new("/bin/sh"), Duration::from_secs(5))
            .expect("sh should report its environment");
        assert!(env.get("PATH").is_some());
    }

    #[test]
    fn a_hanging_shell_is_given_up_on() {
        let dir = tempfile::tempdir().unwrap();
        let shell = dir.path().join("slow-shell");
        std::fs::write(&shell, "#!/bin/sh\nsleep 10\n").unwrap();
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&shell, std::fs::Permissions::from_mode(0o755)).unwrap();
        let started = Instant::now();
        assert!(capture_login_shell(&shell, Duration::from_millis(300)).is_none());
        assert!(started.elapsed() < Duration::from_secs(3));
    }
}
