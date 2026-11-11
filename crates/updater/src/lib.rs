//! Updates from GitHub releases. Gleipnir asks GitHub for the latest release; when it's newer,
//! downloads this platform's installer, checks it against the checksum published with it, and
//! puts it in place of the installed program. The new version runs after a restart, which is
//! always the person's choice: a restart ends their terminals and agents.
//!
//! Only an installed Gleipnir updates itself: an app bundle on macOS, or the program `install.sh`
//! put somewhere this user can write on Linux. One built from source is left alone.

mod install;
mod release;
mod version;

pub use install::Install;
pub use release::Release;

use anyhow::{Context as _, Result, bail};
use gleipnir_workspace::Workspace;
use gpui::{App, Context, Window, actions};
use std::io::{Read as _, Write as _};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

actions!(updater, [CheckForUpdates, RestartToUpdate]);

/// Where releases are published.
const LATEST_RELEASE: &str = "https://api.github.com/repos/Setekh/gleipnir/releases/latest";

/// The first check waits for the window to be up; later ones are far apart, as releases are.
const FIRST_CHECK: Duration = Duration::from_secs(20);
const CHECK_EVERY: Duration = Duration::from_secs(6 * 60 * 60);

/// How long the answer to Check for Updates stays in the status bar.
const ANSWER_SHOWS_FOR: Duration = Duration::from_secs(12);

#[derive(Clone, Debug, PartialEq)]
pub enum Status {
    Idle,
    Checking,
    UpToDate,
    /// The version, and how much of it has arrived, from 0 to 1.
    Downloading(String, f32),
    Installing(String),
    /// Installed, and waiting for a restart.
    Ready(String),
    Failed(String),
}

struct State {
    status: Status,
    busy: bool,
    /// When an answer that only shows for a while goes away.
    until: Option<Instant>,
}

static STATE: Mutex<State> = Mutex::new(State {
    status: Status::Idle,
    busy: false,
    until: None,
});

fn state() -> std::sync::MutexGuard<'static, State> {
    STATE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

pub fn status() -> Status {
    let mut state = state();
    if state.until.is_some_and(|until| Instant::now() >= until) {
        state.until = None;
        state.status = Status::Idle;
    }
    state.status.clone()
}

fn set_status(status: Status) {
    let mut state = state();
    state.until = matches!(status, Status::UpToDate | Status::Failed(_))
        .then(|| Instant::now() + ANSWER_SHOWS_FOR);
    state.status = status;
}

/// What an update needs to know, which tests replace.
pub struct Source {
    /// GitHub's address for the latest release.
    pub latest: String,
    /// The version running now.
    pub current: String,
    pub install: Install,
    /// Where the installer is kept while it downloads.
    pub downloads: PathBuf,
}

impl Source {
    fn running() -> Result<Source> {
        let exe = std::env::current_exe().context("can't tell where Gleipnir runs from")?;
        Ok(Source {
            latest: LATEST_RELEASE.into(),
            current: env!("CARGO_PKG_VERSION").into(),
            install: Install::of(&exe)?,
            downloads: gleipnir_packages::dirs::state_dir().join("updates"),
        })
    }
}

fn agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .user_agent(concat!("gleipnir/", env!("CARGO_PKG_VERSION")))
        .timeout_connect(Some(Duration::from_secs(20)))
        .build()
        .into()
}

fn fetch_text(agent: &ureq::Agent, url: &str) -> Result<String> {
    agent
        .get(url)
        .header("Accept", "application/vnd.github+json")
        .call()
        .and_then(|mut response| response.body_mut().read_to_string())
        .with_context(|| format!("couldn't read {url}"))
}

/// The latest release, when it's newer than the one running. No releases at all is not an error.
pub fn newer_release(source: &Source) -> Result<Option<Release>> {
    let agent = agent();
    let text = match agent
        .get(&source.latest)
        .header("Accept", "application/vnd.github+json")
        .call()
    {
        Ok(mut response) => response.body_mut().read_to_string()?,
        Err(ureq::Error::StatusCode(404)) => return Ok(None),
        Err(error) => return Err(error).context("couldn't ask GitHub for the latest release"),
    };
    let json: serde_json::Value =
        serde_json::from_str(&text).context("GitHub's answer wasn't a release")?;
    let tag = json["tag_name"].as_str().unwrap_or_default();
    if !version::newer(tag, &source.current) {
        return Ok(None);
    }
    release::parse(&json, &source.install.suffix()).map(Some)
}

/// Downloads the release's installer, refusing one that doesn't match its published checksum.
fn download(release: &Release, to: &Path, progress: &dyn Fn(f32)) -> Result<()> {
    use sha2::{Digest as _, Sha256};
    let agent = agent();
    let expected = release::checksum(&fetch_text(&agent, &release.checksum_url)?)?;
    let mut response = agent
        .get(&release.url)
        .call()
        .with_context(|| format!("couldn't download {}", release.asset))?;
    let mut body = response.body_mut().with_config().limit(u64::MAX).reader();
    let mut file = std::fs::File::create(to)?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; 1 << 20];
    let mut done = 0u64;
    loop {
        let read = body
            .read(&mut buffer)
            .with_context(|| format!("the download of {} was cut short", release.asset))?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
        file.write_all(&buffer[..read])?;
        done += read as u64;
        if release.size > 0 {
            progress((done as f32 / release.size as f32).min(1.));
        }
    }
    file.flush()?;
    let actual: String = hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    if actual != expected {
        bail!(
            "{} didn't match its checksum, so it wasn't installed",
            release.asset
        );
    }
    Ok(())
}

/// Checks for a newer release and installs it, reporting each step. Returns the version
/// installed, or none when this one is the latest.
pub fn update(source: &Source, report: &dyn Fn(Status)) -> Result<Option<String>> {
    report(Status::Checking);
    let Some(release) = newer_release(source)? else {
        return Ok(None);
    };
    let version = release.version.clone();
    report(Status::Downloading(version.clone(), 0.));
    std::fs::create_dir_all(&source.downloads)?;
    let installer = source.downloads.join(&release.asset);
    let result = download(&release, &installer, &|done| {
        report(Status::Downloading(version.clone(), done))
    })
    .and_then(|()| {
        report(Status::Installing(version.clone()));
        source.install.apply(&installer)
    });
    std::fs::remove_file(&installer).ok();
    result.map(|()| Some(version))
}

/// Starts a check in the background, unless one is running or an update already waits for a
/// restart. One the person asked for answers in the status bar whatever it finds; the periodic
/// one only speaks up when there's an update.
pub fn check(asked: bool) {
    {
        let mut state = state();
        if state.busy || matches!(state.status, Status::Ready(_)) {
            return;
        }
        state.busy = true;
    }
    std::thread::spawn(move || {
        let result = Source::running().and_then(|source| {
            update(&source, &|status| {
                // The periodic check stays quiet until it has something to install.
                if asked || status != Status::Checking {
                    set_status(status)
                }
            })
        });
        state().busy = false;
        match result {
            Ok(Some(version)) => set_status(Status::Ready(version)),
            Ok(None) if asked => set_status(Status::UpToDate),
            Ok(None) => set_status(Status::Idle),
            Err(error) if asked => set_status(Status::Failed(format!("{error:#}"))),
            Err(error) => {
                eprintln!("update check: {error:#}");
                set_status(Status::Idle);
            }
        }
    });
}

/// Registers the actions, and starts the periodic check.
pub fn init(cx: &mut App) {
    cx.on_action(|_: &CheckForUpdates, _| check(true));
    cx.on_action(|_: &RestartToUpdate, cx| {
        if matches!(status(), Status::Ready(_)) {
            cx.restart();
        }
    });
    // On Linux the running program's file was replaced, so restart by its path from now, not by
    // asking later where the (since deleted) file was.
    #[cfg(target_os = "linux")]
    if let Ok(exe) = std::env::current_exe() {
        cx.set_restart_path(exe);
    }
    cx.spawn(async move |cx| {
        cx.background_executor().timer(FIRST_CHECK).await;
        loop {
            if cx.update(|cx| gleipnir_core::settings(cx).updates.automatic) {
                check(false);
            }
            cx.background_executor().timer(CHECK_EVERY).await;
        }
    })
    .detach();
}

/// What the status bar says for a status, and whether it's the restart button.
fn label(status: &Status) -> Option<(String, bool)> {
    Some(match status {
        Status::Idle => return None,
        Status::Checking => ("Checking for updates…".into(), false),
        Status::UpToDate => (
            format!(
                "Gleipnir {} is the latest version",
                env!("CARGO_PKG_VERSION")
            ),
            false,
        ),
        Status::Downloading(version, done) => (
            format!("Downloading Gleipnir {version} {:.0}%", done * 100.),
            false,
        ),
        Status::Installing(version) => (format!("Installing Gleipnir {version}…"), false),
        Status::Ready(version) => (format!("Restart to update to {version}"), true),
        Status::Failed(why) => (format!("Update: {why}"), false),
    })
}

pub fn add_to_workspace(workspace: &mut Workspace, _: &mut Window, cx: &mut Context<Workspace>) {
    workspace.add_status_item(|_, cx| {
        use gpui::{
            InteractiveElement as _, IntoElement as _, ParentElement as _,
            StatefulInteractiveElement as _, Styled as _,
        };
        let theme = cx.global::<gleipnir_ui::Theme>().clone();
        let status = status();
        let (text, restart) = label(&status)?;
        let item = gpui::div()
            .id("update-status")
            .max_w(gpui::px(420.))
            .truncate()
            .child(text);
        Some(if restart {
            item.text_color(theme.accent)
                .cursor_pointer()
                .on_click(|_, window, cx| window.dispatch_action(Box::new(RestartToUpdate), cx))
                .into_any_element()
        } else if matches!(status, Status::Failed(_)) {
            item.text_color(theme.warning).into_any_element()
        } else {
            item.text_color(theme.text_muted).into_any_element()
        })
    });
    // The status changes on another thread, so the bar is redrawn when it has.
    cx.spawn(async move |this, cx| {
        let mut shown = status();
        loop {
            cx.background_executor()
                .timer(Duration::from_millis(500))
                .await;
            let now = status();
            let changed = now != shown;
            shown = now;
            if this
                .update(cx, |_, cx| {
                    if changed {
                        cx.notify()
                    }
                })
                .is_err()
            {
                return;
            }
        }
    })
    .detach();
}

#[cfg(test)]
mod tests;
