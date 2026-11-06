//! Running a program for its output: builds before a launch, and Run without the debugger.

use anyhow::{Context as _, Result};
use futures::channel::mpsc;
use std::io::Read;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::thread;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProcessOutput {
    Stdout(String),
    Stderr(String),
    /// The exit code, or none if a signal ended it.
    Exited(Option<i32>),
}

#[derive(Clone, Debug)]
pub struct ProcessCommand {
    pub program: PathBuf,
    pub args: Vec<String>,
    pub env: Vec<(String, String)>,
    pub cwd: PathBuf,
}

/// A running process. Dropping it doesn't stop it; `kill` does.
#[derive(Clone)]
pub struct ProcessHandle {
    child: Arc<Mutex<Option<Child>>>,
    stdin: Arc<Mutex<Option<std::process::ChildStdin>>>,
    pid: u32,
}

impl ProcessHandle {
    /// Stops the process and everything it started, as `go run` and `npm run` start the real
    /// program as a child: politely first, then not.
    pub fn kill(&self) {
        if self.child.lock().expect("the child's lock").is_none() {
            return;
        }
        signal_group(self.pid, false);
        let child = self.child.clone();
        let pid = self.pid;
        thread::spawn(move || {
            thread::sleep(std::time::Duration::from_millis(500));
            if child.lock().expect("the child's lock").is_some() {
                signal_group(pid, true);
            }
        });
    }

    pub fn pid(&self) -> u32 {
        self.pid
    }

    /// Types into the program, as a terminal would.
    pub fn write_input(&self, text: &str) -> Result<()> {
        use std::io::Write as _;
        let mut stdin = self.stdin.lock().expect("the stdin lock");
        let Some(pipe) = stdin.as_mut() else {
            anyhow::bail!("the program has finished");
        };
        pipe.write_all(text.as_bytes())?;
        pipe.flush()?;
        Ok(())
    }
}

#[cfg(unix)]
fn signal_group(pid: u32, force: bool) {
    let signal = if force { libc::SIGKILL } else { libc::SIGTERM };
    // The process leads its own group, so this reaches its children too.
    unsafe {
        libc::killpg(pid as libc::pid_t, signal);
    }
}

#[cfg(not(unix))]
fn signal_group(_: u32, _: bool) {}

/// Reads a stream in chunks as they come, so a prompt without a newline still shows.
fn forward(
    mut reader: impl Read + Send + 'static,
    tx: mpsc::UnboundedSender<ProcessOutput>,
    wrap: fn(String) -> ProcessOutput,
) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        let mut buffer = [0u8; 8192];
        // Bytes of a character split across two reads wait for the rest.
        let mut pending: Vec<u8> = Vec::new();
        loop {
            match reader.read(&mut buffer) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    pending.extend_from_slice(&buffer[..n]);
                    let valid = match std::str::from_utf8(&pending) {
                        Ok(_) => pending.len(),
                        Err(error) if error.error_len().is_none() => error.valid_up_to(),
                        Err(_) => pending.len(),
                    };
                    let text = String::from_utf8_lossy(&pending[..valid]).into_owned();
                    pending.drain(..valid);
                    if !text.is_empty() && tx.unbounded_send(wrap(text)).is_err() {
                        break;
                    }
                }
            }
        }
        if !pending.is_empty() {
            tx.unbounded_send(wrap(String::from_utf8_lossy(&pending).into_owned()))
                .ok();
        }
    })
}

pub fn spawn(
    command: &ProcessCommand,
) -> Result<(ProcessHandle, mpsc::UnboundedReceiver<ProcessOutput>)> {
    let mut process = Command::new(&command.program);
    #[cfg(unix)]
    std::os::unix::process::CommandExt::process_group(&mut process, 0);
    let mut child = process
        .args(&command.args)
        .env_clear()
        .envs(command.env.iter().cloned())
        .current_dir(&command.cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("couldn't start {}", command.program.display()))?;
    let (tx, rx) = mpsc::unbounded();
    let stdout = forward(
        child.stdout.take().expect("stdout is piped"),
        tx.clone(),
        ProcessOutput::Stdout,
    );
    let stderr = forward(
        child.stderr.take().expect("stderr is piped"),
        tx.clone(),
        ProcessOutput::Stderr,
    );
    let pid = child.id();
    let stdin = Arc::new(Mutex::new(child.stdin.take()));
    let child = Arc::new(Mutex::new(Some(child)));
    let waited = child.clone();
    thread::spawn(move || {
        // Output first, so the exit is the last thing said.
        stdout.join().ok();
        stderr.join().ok();
        let code = loop {
            let mut guard = waited.lock().expect("the child's lock");
            let Some(child) = guard.as_mut() else {
                break None;
            };
            match child.try_wait() {
                Ok(Some(status)) => {
                    guard.take();
                    break status.code();
                }
                Ok(None) => {
                    drop(guard);
                    thread::sleep(std::time::Duration::from_millis(20));
                }
                Err(_) => break None,
            }
        };
        tx.unbounded_send(ProcessOutput::Exited(code)).ok();
    });
    Ok((ProcessHandle { child, stdin, pid }, rx))
}

/// A running process, as Attach to Process lists it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProcessInfo {
    pub pid: u32,
    /// The executable's file name.
    pub name: String,
    /// The whole command line.
    pub command: String,
}

/// The user's own processes, newest first, leaving out this one.
pub fn list_processes() -> Vec<ProcessInfo> {
    let Ok(output) = Command::new("ps")
        .args(["x", "-o", "pid=", "-o", "args="])
        .output()
    else {
        return Vec::new();
    };
    let own = std::process::id();
    let mut found: Vec<ProcessInfo> = String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|line| {
            let line = line.trim_start();
            let (pid, command) = line.split_once(char::is_whitespace)?;
            let pid: u32 = pid.parse().ok()?;
            let command = command.trim().to_string();
            let program = command.split_whitespace().next()?;
            let name = program.rsplit('/').next().unwrap_or(program).to_string();
            (pid != own).then_some(ProcessInfo { pid, name, command })
        })
        .collect();
    found.sort_by_key(|p| std::cmp::Reverse(p.pid));
    found
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::StreamExt as _;

    fn sh(script: &str) -> ProcessCommand {
        ProcessCommand {
            program: "/bin/sh".into(),
            args: vec!["-c".into(), script.into()],
            env: vec![("PATH".into(), "/usr/bin:/bin".into())],
            cwd: std::env::temp_dir(),
        }
    }

    fn collect(rx: mpsc::UnboundedReceiver<ProcessOutput>) -> (String, String, Option<i32>) {
        let all: Vec<ProcessOutput> = futures::executor::block_on(rx.collect());
        let mut out = (String::new(), String::new(), None);
        for item in all {
            match item {
                ProcessOutput::Stdout(text) => out.0.push_str(&text),
                ProcessOutput::Stderr(text) => out.1.push_str(&text),
                ProcessOutput::Exited(code) => out.2 = code,
            }
        }
        out
    }

    #[test]
    fn output_and_exit_code_arrive_with_the_exit_last() {
        let (_, rx) = spawn(&sh("printf 'one\\ntwo'; printf oops >&2; exit 3")).unwrap();
        assert_eq!(collect(rx), ("one\ntwo".into(), "oops".into(), Some(3)));
    }

    #[test]
    fn a_killed_process_reports_no_exit_code() {
        let (handle, rx) = spawn(&sh("echo started; sleep 30")).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(100));
        handle.kill();
        let (stdout, _, code) = collect(rx);
        assert_eq!(stdout, "started\n");
        assert_eq!(code, None);
    }

    #[test]
    fn input_reaches_the_program() {
        let (handle, rx) = spawn(&sh("read name; echo hello $name")).unwrap();
        handle.write_input("gleipnir\n").unwrap();
        assert_eq!(collect(rx).0, "hello gleipnir\n");
    }

    #[test]
    fn running_processes_are_listed_with_their_names() {
        let (handle, _rx) = spawn(&sh("sleep 5")).unwrap();
        let listed = list_processes();
        handle.kill();
        let ours = listed
            .iter()
            .find(|p| p.pid == handle.pid())
            .expect("the process we started");
        // `sh -c` may replace itself with the command it runs.
        assert!(["sh", "sleep"].contains(&ours.name.as_str()), "{ours:?}");
        assert!(ours.command.contains("sleep 5"));
        assert!(!listed.iter().any(|p| p.pid == std::process::id()));
    }

    #[test]
    fn a_missing_program_fails_to_start() {
        let mut command = sh("");
        command.program = "/no/such/program".into();
        assert!(spawn(&command).is_err());
    }
}
