//! Remembers each project's layout between sessions. Kept outside the project, so opening a folder
//! never writes anything into it.

use crate::dock::DockPosition;
use crate::pane_tree::{Axis, PaneTree};
use anyhow::{Context as _, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

pub use gleipnir_packages::{config_dir, state_dir};

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct SavedTab {
    pub path: PathBuf,
    #[serde(default)]
    pub cursor: usize,
    #[serde(default)]
    pub scroll_y: f32,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct SavedPane {
    pub tabs: Vec<SavedTab>,
    #[serde(default)]
    pub active: usize,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum SavedTree {
    Pane(SavedPane),
    Split {
        axis: Axis,
        children: Vec<SavedTree>,
        ratios: Vec<f32>,
    },
}

impl SavedTree {
    pub fn to_tree(&self) -> PaneTree<SavedPane> {
        match self {
            Self::Pane(pane) => PaneTree::Pane(pane.clone()),
            Self::Split {
                axis,
                children,
                ratios,
            } => PaneTree::Split {
                axis: *axis,
                children: children.iter().map(Self::to_tree).collect(),
                ratios: ratios.clone(),
            },
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SavedDock {
    pub position: DockPosition,
    pub visible: bool,
    pub size: f32,
    #[serde(default)]
    pub active_panel: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SavedWorkspace {
    pub center: SavedTree,
    #[serde(default)]
    pub active_pane: usize,
    #[serde(default)]
    pub docks: Vec<SavedDock>,
    #[serde(default)]
    pub panels: HashMap<String, serde_json::Value>,
    #[serde(default)]
    pub recent_files: Vec<PathBuf>,
    #[serde(default)]
    pub recent_actions: Vec<String>,
    /// The SDK chosen for each kind, by its root folder.
    #[serde(default)]
    pub sdks: std::collections::BTreeMap<String, PathBuf>,
}

/// A file name for a project's state, readable enough to find by hand.
fn file_name_for(root: &Path) -> String {
    let mut name = root.to_string_lossy().replace(['/', '\\', ':'], "-");
    name.push_str(".json");
    name
}

pub(crate) fn state_path(root: &Path) -> PathBuf {
    state_dir().join("workspaces").join(file_name_for(root))
}

/// Reads a project's saved layout. A missing or unreadable one just means starting fresh: an old or
/// damaged state file should never stop a project from opening.
pub(crate) fn load(root: &Path) -> Option<SavedWorkspace> {
    let text = fs::read_to_string(state_path(root)).ok()?;
    serde_json::from_str(&text).ok()
}

/// Writes via a temporary file, so a crash mid-write can't leave a damaged one.
pub(crate) fn save(root: &Path, state: &SavedWorkspace) -> Result<()> {
    write_json(&state_path(root), state)
}

fn write_json<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    let directory = path.parent().context("the state path has no folder")?;
    fs::create_dir_all(directory)?;
    let temporary = path.with_extension("json.tmp");
    fs::write(&temporary, serde_json::to_vec_pretty(value)?)?;
    fs::rename(&temporary, path)?;
    Ok(())
}

fn last_project_path() -> PathBuf {
    state_dir().join("last-project")
}

pub fn last_project() -> Option<PathBuf> {
    let text = fs::read_to_string(last_project_path()).ok()?;
    let path = PathBuf::from(text.trim());
    path.is_dir().then_some(path)
}

pub(crate) fn remember_last_project(root: &Path) -> Result<()> {
    fs::create_dir_all(state_dir())?;
    fs::write(last_project_path(), root.to_string_lossy().as_bytes())?;
    let mut recent = recent_projects();
    recent.retain(|p| p != root);
    recent.insert(0, root.to_path_buf());
    recent.truncate(RECENT_PROJECTS);
    write_json(&recent_projects_path(), &recent)
}

/// How many projects the project menu offers.
const RECENT_PROJECTS: usize = 15;

fn recent_projects_path() -> PathBuf {
    state_dir().join("recent-projects.json")
}

/// Projects opened before, most recent first, leaving out any that have gone.
pub fn recent_projects() -> Vec<PathBuf> {
    let saved: Vec<PathBuf> = fs::read(recent_projects_path())
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default();
    let mut recent: Vec<PathBuf> = saved.into_iter().filter(|p| p.is_dir()).collect();
    // From before the list was kept: the last project is at least known.
    if recent.is_empty()
        && let Some(last) = last_project()
    {
        recent.push(last);
    }
    recent
}

/// The project a file belongs to: the nearest folder above it that's a repository or a Unity
/// project, which is often a folder in one, or failing that the folder it's in.
pub fn project_root_for(path: &Path) -> Option<PathBuf> {
    let start = if path.is_dir() { path } else { path.parent()? };
    start
        .ancestors()
        .find(|dir| dir.join(".git").exists() || gleipnir_index::unity::is_unity_project(dir))
        .map(Path::to_path_buf)
        .or_else(|| Some(start.to_path_buf()))
}

#[cfg(test)]
mod persistence_tests {
    use super::*;

    #[test]
    fn a_unity_project_in_a_repository_is_its_own_project() {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path();
        std::fs::create_dir_all(repo.join(".git")).unwrap();
        let game = repo.join("Game");
        std::fs::create_dir_all(game.join("ProjectSettings")).unwrap();
        std::fs::write(game.join("ProjectSettings/ProjectVersion.txt"), "").unwrap();
        std::fs::create_dir_all(game.join("Assets/Scripts")).unwrap();
        let script = game.join("Assets/Scripts/Player.cs");
        std::fs::write(&script, "").unwrap();
        assert_eq!(project_root_for(&script), Some(game));
        std::fs::write(repo.join("README.md"), "").unwrap();
        assert_eq!(
            project_root_for(&repo.join("README.md")),
            Some(repo.to_path_buf())
        );
    }

    #[test]
    fn state_file_names_are_flat() {
        assert_eq!(
            file_name_for(Path::new("/Users/me/dev/app")),
            "-Users-me-dev-app.json"
        );
    }

    #[test]
    fn layouts_survive_a_round_trip_including_older_shapes() {
        let state = SavedWorkspace {
            center: SavedTree::Split {
                axis: Axis::Horizontal,
                children: vec![
                    SavedTree::Pane(SavedPane {
                        tabs: vec![SavedTab {
                            path: "a.rs".into(),
                            cursor: 3,
                            scroll_y: 40.,
                        }],
                        active: 0,
                    }),
                    SavedTree::Pane(SavedPane::default()),
                ],
                ratios: vec![0.3, 0.7],
            },
            active_pane: 1,
            docks: vec![SavedDock {
                position: DockPosition::Left,
                visible: true,
                size: 280.,
                active_panel: Some("project".into()),
            }],
            panels: HashMap::new(),
            recent_files: vec!["a.rs".into()],
            recent_actions: vec!["editor::Undo".into()],
            sdks: std::collections::BTreeMap::from([("go".to_string(), PathBuf::from("/go"))]),
        };
        let json = serde_json::to_string(&state).unwrap();
        assert_eq!(
            serde_json::from_str::<SavedWorkspace>(&json).unwrap(),
            state
        );

        // Fields added later must not break files written before they existed.
        let old = r#"{"center":{"Pane":{"tabs":[{"path":"a.rs"}]}}}"#;
        let parsed: SavedWorkspace = serde_json::from_str(old).unwrap();
        assert!(parsed.docks.is_empty());
    }

    #[test]
    fn the_saved_tree_converts_to_the_real_shape() {
        let saved = SavedTree::Split {
            axis: Axis::Vertical,
            children: vec![
                SavedTree::Pane(SavedPane::default()),
                SavedTree::Pane(SavedPane::default()),
            ],
            ratios: vec![0.5, 0.5],
        };
        assert_eq!(saved.to_tree().panes().len(), 2);
    }

    #[test]
    fn the_project_root_is_the_nearest_repository_or_else_the_folder() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("repo");
        fs::create_dir_all(root.join(".git")).unwrap();
        fs::create_dir_all(root.join("src/deep")).unwrap();
        let file = root.join("src/deep/a.rs");
        fs::write(&file, "").unwrap();
        assert_eq!(project_root_for(&file), Some(root.clone()));
        assert_eq!(project_root_for(&root.join("src")), Some(root));

        let plain = dir.path().join("plain");
        fs::create_dir_all(&plain).unwrap();
        let loose = plain.join("x.txt");
        fs::write(&loose, "").unwrap();
        // tempdirs aren't inside a repository, unless the machine's temp dir somehow is.
        assert_eq!(project_root_for(&loose), Some(plain));
    }
}
