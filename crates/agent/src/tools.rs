//! The tools an agent is offered: what each is called, what it's for and what it takes, and its
//! arguments checked and turned into a `ToolCall` before anything in the IDE is touched.

use serde_json::{Value, json};
use std::path::{Component, Path, PathBuf};
use std::time::Duration;

/// How long a tool that waits for something, like tests or a person's review, waits by default
/// before answering with what it has. Agents give up on tool calls after a while (Codex after a
/// minute unless configured otherwise), so this stays under that, and they can ask again.
pub const DEFAULT_WAIT: Duration = Duration::from_secs(50);
/// The longest a tool will wait, whatever it's asked.
pub const MAX_WAIT: Duration = Duration::from_secs(600);
/// How long running a configuration waits for it to finish before answering that it's running.
pub const DEFAULT_RUN_WAIT: Duration = Duration::from_secs(10);

struct Tool {
    name: &'static str,
    title: &'static str,
    description: &'static str,
    /// Whether it only looks, changing nothing.
    read_only: bool,
    schema: fn() -> Value,
}

fn path_property(what: &str) -> Value {
    json!({
        "type": "string",
        "description": format!("{what}, relative to the project root (an absolute path inside the project also works)"),
    })
}

fn wait_property(default: Duration) -> Value {
    json!({
        "type": "number",
        "minimum": 0,
        "maximum": MAX_WAIT.as_secs(),
        "description": format!("How many seconds to wait before answering with what there is so far (default {}).", default.as_secs()),
    })
}

fn no_arguments() -> Value {
    json!({ "type": "object", "properties": {}, "additionalProperties": false })
}

fn position_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "path": path_property("The file"),
            "line": { "type": "integer", "minimum": 1, "description": "Line, from 1" },
            "column": { "type": "integer", "minimum": 1, "description": "Column, from 1, counting characters; the name's start, or anywhere in it" },
            "symbol": { "type": "string", "description": "A name to look up instead of a position, such as a function or type name" },
        },
        "additionalProperties": false,
    })
}

fn edit_schema(extra: Option<(&str, Value)>) -> Value {
    let mut schema = json!({
        "type": "object",
        "properties": {
            "path": path_property("The file to change"),
            "old_text": { "type": "string", "description": "Text in the file to replace, exactly as it is, including indentation. It must appear once, unless replace_all is set; include more of the lines around it to make it unique." },
            "new_text": { "type": "string", "description": "What replaces it" },
            "replace_all": { "type": "boolean", "description": "Replace every occurrence of old_text rather than exactly one (default false)" },
        },
        "required": ["path", "old_text", "new_text"],
        "additionalProperties": false,
    });
    if let Some((name, property)) = extra {
        schema["properties"][name] = property;
    }
    schema
}

const TOOLS: &[Tool] = &[
    Tool {
        name: "get_open_files",
        title: "Open files",
        description: "The files open in Gleipnir's editor tabs: their paths, which one the person is looking at (active), and which have unsaved edits (dirty).",
        read_only: true,
        schema: no_arguments,
    },
    Tool {
        name: "get_selection",
        title: "Selection",
        description: "What the person has selected in the active editor: the file, the selected text with its line range, and the cursor. With nothing selected, the cursor's line instead. Call this first when the person says \"this\", \"here\" or \"the selection\".",
        read_only: true,
        schema: no_arguments,
    },
    Tool {
        name: "get_problems",
        title: "Problems",
        description: "Errors and warnings from the language servers, as the Problems panel shows them, for one file or for every file they've reported on. Positions follow unsaved edits in open files.",
        read_only: true,
        schema: || {
            json!({
                "type": "object",
                "properties": {
                    "path": path_property("Only this file's problems"),
                    "errors_only": { "type": "boolean", "description": "Leave out warnings, information and hints (default false)" },
                },
                "additionalProperties": false,
            })
        },
    },
    Tool {
        name: "get_test_results",
        title: "Test results",
        description: "The results of the latest test run in Gleipnir's test runner, whether the person or run_tests started it: whether it passed, the counts, and each failed test with its file, line and output.",
        read_only: true,
        schema: no_arguments,
    },
    Tool {
        name: "get_git_status",
        title: "Git status",
        description: "The project's Git branch, the files changed since the last commit with how each changed, and any merge or rebase in progress.",
        read_only: true,
        schema: no_arguments,
    },
    Tool {
        name: "find_definition",
        title: "Find definition",
        description: "Where something is declared, from the language server: give a position (path, line, column) on a name, or a symbol name to look up in the project. Each result has its path, line and the line's text.",
        read_only: true,
        schema: position_schema,
    },
    Tool {
        name: "find_usages",
        title: "Find usages",
        description: "Everywhere something is used, from the language server, more precisely than a text search: give a position (path, line, column) on a name, or a symbol name. Each result has its path, line and the line's text.",
        read_only: true,
        schema: position_schema,
    },
    Tool {
        name: "read_file",
        title: "Read file",
        description: "A file's text as Gleipnir has it: from its editor tab if it's open, including edits the person hasn't saved yet, or else from disk. Optionally only some lines. The first part of the answer describes it; the second is the text itself.",
        read_only: true,
        schema: || {
            json!({
                "type": "object",
                "properties": {
                    "path": path_property("The file"),
                    "start_line": { "type": "integer", "minimum": 1, "description": "First line to read, from 1" },
                    "end_line": { "type": "integer", "minimum": 1, "description": "Last line to read, included" },
                },
                "required": ["path"],
                "additionalProperties": false,
            })
        },
    },
    Tool {
        name: "open_file",
        title: "Open file",
        description: "Shows a file in Gleipnir's editor, at a line and column if given, optionally selecting up to an end position, to show the person something. Keyboard focus stays where it is.",
        read_only: false,
        schema: || {
            json!({
                "type": "object",
                "properties": {
                    "path": path_property("The file"),
                    "line": { "type": "integer", "minimum": 1, "description": "Line to put the cursor on, from 1" },
                    "column": { "type": "integer", "minimum": 1, "description": "Column, from 1, counting characters (default 1)" },
                    "end_line": { "type": "integer", "minimum": 1, "description": "Selects from line and column to here" },
                    "end_column": { "type": "integer", "minimum": 1, "description": "Column the selection ends before (default: the end of end_line)" },
                },
                "required": ["path"],
                "additionalProperties": false,
            })
        },
    },
    Tool {
        name: "edit_file",
        title: "Edit file",
        description: "Replaces text in a file inside Gleipnir's editor, as one step the person can undo with Cmd+Z. The file is opened in a tab if it isn't already; its other unsaved edits are kept, and this change stays unsaved too until save_file or the person saves. Prefer this to writing a file that's open in Gleipnir, which would conflict with unsaved edits. Fails, changing nothing, if old_text isn't found or is found more than once without replace_all.",
        read_only: false,
        schema: || edit_schema(None),
    },
    Tool {
        name: "save_file",
        title: "Save file",
        description: "Saves a file open in Gleipnir to disk, with whatever edits it has.",
        read_only: false,
        schema: || {
            json!({
                "type": "object",
                "properties": { "path": path_property("The file to save") },
                "required": ["path"],
                "additionalProperties": false,
            })
        },
    },
    Tool {
        name: "run_tests",
        title: "Run tests",
        description: "Runs tests with Gleipnir's test runner, as the person would from the gutter: a file's tests, the test declared on a line, or a test by name. Answers with the results once the run ends; if it takes longer than wait_seconds, answers that it's still running, and get_test_results has the rest later. The person sees the run in the Run panel.",
        read_only: false,
        schema: || {
            json!({
                "type": "object",
                "properties": {
                    "path": path_property("The test file, or the file a test is in"),
                    "test": { "type": "string", "description": "A test's name, like `parses_empty_input`, `tests::parses_empty_input`, `TestParse` or a JavaScript test's title" },
                    "line": { "type": "integer", "minimum": 1, "description": "A line a test is declared on, from 1, instead of its name" },
                    "wait_seconds": wait_property(DEFAULT_WAIT),
                },
                "required": ["path"],
                "additionalProperties": false,
            })
        },
    },
    Tool {
        name: "run_configuration",
        title: "Run configuration",
        description: "Lists the project's run configurations (Cargo binaries, Go and Odin main packages, package.json scripts) when given no name, or runs one by name, as the Run button does, answering with its status and output so far. A program still running after wait_seconds keeps running in the Run panel.",
        read_only: false,
        schema: || {
            json!({
                "type": "object",
                "properties": {
                    "name": { "type": "string", "description": "The configuration to run, as listed" },
                    "wait_seconds": wait_property(DEFAULT_RUN_WAIT),
                },
                "additionalProperties": false,
            })
        },
    },
    Tool {
        name: "propose_edit",
        title: "Propose edit",
        description: "Offers a change for the person to review before anything changes: Gleipnir shows it as a diff, and they accept or reject each change, or all of them. Takes the same arguments as edit_file. Waits up to wait_seconds for their decision; if they haven't finished by then, answers with status \"pending\" and an id to pass to get_proposal. Accepted changes land in the open file, unsaved, like edit_file's.",
        read_only: false,
        schema: || edit_schema(Some(("wait_seconds", wait_property(DEFAULT_WAIT)))),
    },
    Tool {
        name: "get_proposal",
        title: "Proposal",
        description: "What the person decided about an edit offered with propose_edit, by its id, waiting up to wait_seconds for them to finish if they haven't.",
        read_only: true,
        schema: || {
            json!({
                "type": "object",
                "properties": {
                    "id": { "type": "integer", "description": "The id propose_edit answered with" },
                    "wait_seconds": wait_property(DEFAULT_WAIT),
                },
                "required": ["id"],
                "additionalProperties": false,
            })
        },
    },
];

/// Every tool, as `tools/list` describes them.
pub fn list() -> Vec<Value> {
    TOOLS
        .iter()
        .map(|tool| {
            json!({
                "name": tool.name,
                "title": tool.title,
                "description": tool.description,
                "inputSchema": (tool.schema)(),
                "annotations": {
                    "title": tool.title,
                    "readOnlyHint": tool.read_only,
                    "destructiveHint": false,
                    "openWorldHint": false,
                },
            })
        })
        .collect()
}

pub fn is_tool(name: &str) -> bool {
    TOOLS.iter().any(|tool| tool.name == name)
}

/// A file in the project, checked to exist and to be inside it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FilePath {
    pub absolute: PathBuf,
    /// As agents are given paths: from the root, with `/` between folders.
    pub relative: String,
}

/// A place in a file: line and column from 1, the column counting characters.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Position {
    pub line: usize,
    pub column: usize,
}

/// What a lookup is about: a name at a place, or a name alone.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Target {
    At(FilePath, Position),
    Symbol(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Edit {
    pub path: FilePath,
    pub old_text: String,
    pub new_text: String,
    pub replace_all: bool,
}

/// Which tests to run.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TestChoice {
    /// The file's tests, all of them.
    File,
    Named(String),
    /// The test declared on a line, from 1.
    Line(usize),
}

/// A tool call whose arguments make sense.
#[derive(Clone, Debug, PartialEq)]
pub enum ToolCall {
    GetOpenFiles,
    GetSelection,
    GetProblems {
        path: Option<FilePath>,
        errors_only: bool,
    },
    GetTestResults,
    GetGitStatus,
    FindDefinition(Target),
    FindUsages(Target),
    ReadFile {
        path: FilePath,
        start_line: Option<usize>,
        end_line: Option<usize>,
    },
    OpenFile {
        path: FilePath,
        at: Option<Position>,
        /// Where a selection from `at` ends; a column of `None` is the end of the line.
        select_to: Option<(usize, Option<usize>)>,
    },
    EditFile(Edit),
    SaveFile(FilePath),
    RunTests {
        path: FilePath,
        choice: TestChoice,
        wait: Duration,
    },
    RunConfiguration {
        name: Option<String>,
        wait: Duration,
    },
    ProposeEdit {
        edit: Edit,
        wait: Duration,
    },
    GetProposal {
        id: u64,
        wait: Duration,
    },
}

/// Checks a tool's arguments, resolving paths against the project root, which must be canonical.
pub fn parse(name: &str, arguments: &Value, root: &Path) -> Result<ToolCall, String> {
    let args = Args { name, arguments };
    let path = || -> Result<FilePath, String> { resolve_path(root, &args.string("path")?) };
    let call = match name {
        "get_open_files" => ToolCall::GetOpenFiles,
        "get_selection" => ToolCall::GetSelection,
        "get_problems" => ToolCall::GetProblems {
            path: match args.optional_string("path")? {
                Some(given) => Some(resolve_path(root, &given)?),
                None => None,
            },
            errors_only: args.optional_bool("errors_only")?.unwrap_or(false),
        },
        "get_test_results" => ToolCall::GetTestResults,
        "get_git_status" => ToolCall::GetGitStatus,
        "find_definition" => ToolCall::FindDefinition(target(&args, root)?),
        "find_usages" => ToolCall::FindUsages(target(&args, root)?),
        "read_file" => {
            let start_line = args.optional_line("start_line")?;
            let end_line = args.optional_line("end_line")?;
            if let (Some(start), Some(end)) = (start_line, end_line)
                && end < start
            {
                return Err(format!("end_line ({end}) is before start_line ({start})"));
            }
            ToolCall::ReadFile {
                path: path()?,
                start_line,
                end_line,
            }
        }
        "open_file" => {
            let path = path()?;
            let line = args.optional_line("line")?;
            let column = args.optional_line("column")?;
            let end_line = args.optional_line("end_line")?;
            let end_column = args.optional_line("end_column")?;
            if line.is_none() && (column.is_some() || end_line.is_some()) {
                return Err("open_file needs a line to go with column or end_line".into());
            }
            if end_line.is_none() && end_column.is_some() {
                return Err("open_file needs an end_line to go with end_column".into());
            }
            if let (Some(line), Some(end)) = (line, end_line)
                && end < line
            {
                return Err(format!("end_line ({end}) is before line ({line})"));
            }
            ToolCall::OpenFile {
                path,
                at: line.map(|line| Position {
                    line,
                    column: column.unwrap_or(1),
                }),
                select_to: end_line.map(|end| (end, end_column)),
            }
        }
        "edit_file" => ToolCall::EditFile(edit(&args, root)?),
        "save_file" => ToolCall::SaveFile(path()?),
        "run_tests" => {
            let path = path()?;
            let test = args.optional_string("test")?;
            let line = args.optional_line("line")?;
            let choice = match (test, line) {
                (Some(_), Some(_)) => {
                    return Err("give run_tests a test's name or its line, not both".into());
                }
                (Some(test), None) if test.trim().is_empty() => {
                    return Err("test is empty: leave it out to run the whole file".into());
                }
                (Some(test), None) => TestChoice::Named(test.trim().to_string()),
                (None, Some(line)) => TestChoice::Line(line),
                (None, None) => TestChoice::File,
            };
            ToolCall::RunTests {
                path,
                choice,
                wait: args.wait(DEFAULT_WAIT)?,
            }
        }
        "run_configuration" => ToolCall::RunConfiguration {
            name: args.optional_string("name")?,
            wait: args.wait(DEFAULT_RUN_WAIT)?,
        },
        "propose_edit" => ToolCall::ProposeEdit {
            edit: edit(&args, root)?,
            wait: args.wait(DEFAULT_WAIT)?,
        },
        "get_proposal" => ToolCall::GetProposal {
            id: args.unsigned("id")?,
            wait: args.wait(DEFAULT_WAIT)?,
        },
        other => return Err(format!("Unknown tool: {other}")),
    };
    Ok(call)
}

fn target(args: &Args, root: &Path) -> Result<Target, String> {
    if let Some(given) = args.optional_string("path")? {
        let path = resolve_path(root, &given)?;
        let Some(line) = args.optional_line("line")? else {
            return Err(format!("{} needs a line with the path", args.name));
        };
        let column = args.optional_line("column")?.unwrap_or(1);
        return Ok(Target::At(path, Position { line, column }));
    }
    match args.optional_string("symbol")? {
        Some(symbol) if !symbol.trim().is_empty() => Ok(Target::Symbol(symbol.trim().to_string())),
        _ => Err(format!(
            "{} needs either a path and line (and column) on a name, or a symbol",
            args.name
        )),
    }
}

fn edit(args: &Args, root: &Path) -> Result<Edit, String> {
    let path = resolve_path(root, &args.string("path")?)?;
    // Lines end in `\n` in the editor, whatever the file has on disk.
    let old_text = args.raw_string("old_text")?.replace("\r\n", "\n");
    let new_text = args.raw_string("new_text")?.replace("\r\n", "\n");
    if old_text.is_empty() {
        return Err("old_text is empty: give the text to replace".into());
    }
    if old_text == new_text {
        return Err("old_text and new_text are the same, so there's nothing to change".into());
    }
    Ok(Edit {
        path,
        old_text,
        new_text,
        replace_all: args.optional_bool("replace_all")?.unwrap_or(false),
    })
}

struct Args<'a> {
    name: &'a str,
    arguments: &'a Value,
}

impl Args<'_> {
    fn get(&self, key: &str) -> Option<&Value> {
        self.arguments.get(key).filter(|v| !v.is_null())
    }

    fn raw_string(&self, key: &str) -> Result<String, String> {
        match self.get(key) {
            Some(Value::String(s)) => Ok(s.clone()),
            Some(_) => Err(format!("{key} must be a string")),
            None => Err(format!("{} needs {key}", self.name)),
        }
    }

    fn string(&self, key: &str) -> Result<String, String> {
        let value = self.raw_string(key)?;
        if value.trim().is_empty() {
            return Err(format!("{key} is empty"));
        }
        Ok(value)
    }

    fn optional_string(&self, key: &str) -> Result<Option<String>, String> {
        match self.get(key) {
            None => Ok(None),
            Some(_) => self.raw_string(key).map(Some),
        }
    }

    fn optional_bool(&self, key: &str) -> Result<Option<bool>, String> {
        match self.get(key) {
            None => Ok(None),
            Some(Value::Bool(b)) => Ok(Some(*b)),
            Some(_) => Err(format!("{key} must be true or false")),
        }
    }

    fn unsigned(&self, key: &str) -> Result<u64, String> {
        match self.get(key) {
            Some(value) => value
                .as_u64()
                .ok_or_else(|| format!("{key} must be a whole number")),
            None => Err(format!("{} needs {key}", self.name)),
        }
    }

    /// A line or column, which count from 1.
    fn optional_line(&self, key: &str) -> Result<Option<usize>, String> {
        match self.get(key) {
            None => Ok(None),
            Some(value) => match value.as_u64() {
                Some(0) => Err(format!("{key} counts from 1, so it can't be 0")),
                Some(n) => Ok(Some(n as usize)),
                None => Err(format!("{key} must be a whole number from 1")),
            },
        }
    }

    fn wait(&self, default: Duration) -> Result<Duration, String> {
        match self.get("wait_seconds") {
            None => Ok(default),
            Some(value) => match value.as_f64() {
                Some(seconds) if seconds >= 0.0 && seconds.is_finite() => {
                    Ok(Duration::from_secs_f64(seconds).min(MAX_WAIT))
                }
                _ => Err("wait_seconds must be a number of seconds, 0 or more".into()),
            },
        }
    }
}

/// A path an agent gave, as a file in the project. Relative paths are from the root. Anything
/// that leads outside the root, by `..` or through a symbolic link, is refused.
pub fn resolve_path(root: &Path, given: &str) -> Result<FilePath, String> {
    let given = given.trim();
    if given.is_empty() {
        return Err("path is empty".into());
    }
    let joined = if Path::new(given).is_absolute() {
        PathBuf::from(given)
    } else {
        root.join(given)
    };
    let outside = || {
        format!(
            "{given} is outside the project ({}); only files in it can be used",
            root.display()
        )
    };
    let path = match std::fs::canonicalize(&joined) {
        Ok(path) => path,
        // Said plainly when it would be outside anyway, rather than that it doesn't exist.
        Err(_) if !normalize(&joined).starts_with(root) => return Err(outside()),
        Err(_) => return Err(format!("{given} doesn't exist")),
    };
    if !path.starts_with(root) {
        return Err(if normalize(&joined).starts_with(root) {
            format!(
                "{given} is a link to somewhere outside the project; only files in it can be used"
            )
        } else {
            outside()
        });
    }
    if path.is_dir() {
        return Err(format!("{given} is a folder, not a file"));
    }
    Ok(FilePath {
        relative: relative(root, &path),
        absolute: path,
    })
}

/// A path as an agent is shown it: from the root if it's inside, else as it is.
pub fn relative(root: &Path, path: &Path) -> String {
    match path.strip_prefix(root) {
        Ok(inside) => inside
            .components()
            .map(|c| c.as_os_str().to_string_lossy())
            .collect::<Vec<_>>()
            .join("/"),
        Err(_) => path.display().to_string(),
    }
}

/// Takes out `.` and `..` without looking at the disk.
fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn project() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join("src/main.rs"), "fn main() {}\n").unwrap();
        (dir, root)
    }

    fn call(name: &str, arguments: Value, root: &Path) -> Result<ToolCall, String> {
        parse(name, &arguments, root)
    }

    #[test]
    fn paths_inside_the_project_resolve_relative_or_absolute() {
        let (_dir, root) = project();
        let main = root.join("src/main.rs");
        for given in [
            "src/main.rs",
            "./src/../src/main.rs",
            main.to_str().unwrap(),
        ] {
            let path = resolve_path(&root, given).unwrap();
            assert_eq!(path.absolute, main, "{given}");
            assert_eq!(path.relative, "src/main.rs", "{given}");
        }
    }

    #[test]
    fn paths_outside_the_project_are_refused() {
        let (_dir, root) = project();
        let outside = tempfile::tempdir().unwrap();
        let secret = std::fs::canonicalize(outside.path())
            .unwrap()
            .join("secret");
        std::fs::write(&secret, "x").unwrap();
        for given in [
            "../secret".to_string(),
            "src/../../secret".to_string(),
            secret.display().to_string(),
            "/etc/passwd".to_string(),
        ] {
            let error = resolve_path(&root, &given).unwrap_err();
            assert!(error.contains("outside the project"), "{given}: {error}");
        }
        // A link inside that points outside is refused too.
        std::os::unix::fs::symlink(&secret, root.join("link")).unwrap();
        let error = resolve_path(&root, "link").unwrap_err();
        assert!(error.contains("link to somewhere outside"), "{error}");
        assert!(
            resolve_path(&root, "src/missing.rs")
                .unwrap_err()
                .contains("doesn't exist")
        );
        assert!(resolve_path(&root, "src").unwrap_err().contains("folder"));
        assert!(resolve_path(&root, "  ").unwrap_err().contains("empty"));
    }

    #[test]
    fn edits_need_text_to_find_that_differs_from_its_replacement() {
        let (_dir, root) = project();
        let ok = call(
            "edit_file",
            json!({ "path": "src/main.rs", "old_text": "main()\r\n", "new_text": "start()" }),
            &root,
        )
        .unwrap();
        let ToolCall::EditFile(edit) = ok else {
            panic!("{ok:?}")
        };
        assert_eq!(edit.old_text, "main()\n");
        assert!(!edit.replace_all);
        let error = |args: Value| call("edit_file", args, &root).unwrap_err();
        assert!(
            error(json!({ "path": "src/main.rs", "old_text": "", "new_text": "x" }))
                .contains("empty")
        );
        assert!(
            error(json!({ "path": "src/main.rs", "old_text": "a", "new_text": "a" }))
                .contains("the same")
        );
        assert!(
            error(json!({ "path": "src/main.rs", "new_text": "a" })).contains("needs old_text")
        );
        assert!(
            error(json!({ "path": "src/main.rs", "old_text": 1, "new_text": "a" }))
                .contains("must be a string")
        );
        assert!(
            error(json!({ "path": "src/main.rs", "old_text": "a", "new_text": "b", "replace_all": "yes" }))
                .contains("true or false")
        );
        assert!(
            error(json!({ "path": "../x", "old_text": "a", "new_text": "b" })).contains("outside")
        );
        let proposal = call(
            "propose_edit",
            json!({ "path": "src/main.rs", "old_text": "a", "new_text": "b", "wait_seconds": 5 }),
            &root,
        )
        .unwrap();
        assert!(
            matches!(proposal, ToolCall::ProposeEdit { wait, .. } if wait == Duration::from_secs(5))
        );
    }

    #[test]
    fn lookups_take_a_position_or_a_symbol() {
        let (_dir, root) = project();
        let at = call(
            "find_definition",
            json!({ "path": "src/main.rs", "line": 1, "column": 4 }),
            &root,
        )
        .unwrap();
        assert!(matches!(
            at,
            ToolCall::FindDefinition(Target::At(_, Position { line: 1, column: 4 }))
        ));
        let column_defaults = call(
            "find_usages",
            json!({ "path": "src/main.rs", "line": 2 }),
            &root,
        )
        .unwrap();
        assert!(matches!(
            column_defaults,
            ToolCall::FindUsages(Target::At(_, Position { column: 1, .. }))
        ));
        let symbol = call("find_usages", json!({ "symbol": " main " }), &root).unwrap();
        assert_eq!(symbol, ToolCall::FindUsages(Target::Symbol("main".into())));
        assert!(
            call("find_usages", json!({}), &root)
                .unwrap_err()
                .contains("either")
        );
        assert!(
            call("find_usages", json!({ "path": "src/main.rs" }), &root)
                .unwrap_err()
                .contains("needs a line")
        );
        assert!(
            call(
                "find_usages",
                json!({ "path": "src/main.rs", "line": 0 }),
                &root
            )
            .unwrap_err()
            .contains("from 1")
        );
    }

    #[test]
    fn line_ranges_and_waits_are_checked() {
        let (_dir, root) = project();
        assert!(
            call(
                "read_file",
                json!({ "path": "src/main.rs", "start_line": 5, "end_line": 2 }),
                &root
            )
            .unwrap_err()
            .contains("before")
        );
        assert!(
            call(
                "open_file",
                json!({ "path": "src/main.rs", "column": 3 }),
                &root
            )
            .unwrap_err()
            .contains("needs a line")
        );
        let open = call(
            "open_file",
            json!({ "path": "src/main.rs", "line": 2, "end_line": 3 }),
            &root,
        )
        .unwrap();
        assert!(matches!(
            open,
            ToolCall::OpenFile {
                at: Some(Position { line: 2, column: 1 }),
                select_to: Some((3, None)),
                ..
            }
        ));
        let run = call(
            "run_tests",
            json!({ "path": "src/main.rs", "wait_seconds": 100000 }),
            &root,
        )
        .unwrap();
        assert!(
            matches!(run, ToolCall::RunTests { choice: TestChoice::File, wait, .. } if wait == MAX_WAIT)
        );
        assert!(
            call(
                "run_tests",
                json!({ "path": "src/main.rs", "test": "a", "line": 2 }),
                &root
            )
            .unwrap_err()
            .contains("not both")
        );
        assert!(
            call(
                "run_tests",
                json!({ "path": "src/main.rs", "wait_seconds": -1 }),
                &root
            )
            .unwrap_err()
            .contains("wait_seconds")
        );
        let named = call(
            "run_tests",
            json!({ "path": "src/main.rs", "test": "adds" }),
            &root,
        )
        .unwrap();
        assert!(
            matches!(named, ToolCall::RunTests { choice: TestChoice::Named(n), .. } if n == "adds")
        );
        assert!(
            call("get_proposal", json!({}), &root)
                .unwrap_err()
                .contains("needs id")
        );
        assert!(
            call("get_proposal", json!({ "id": "one" }), &root)
                .unwrap_err()
                .contains("whole number")
        );
        let problems = call("get_problems", json!({ "path": null }), &root).unwrap();
        assert_eq!(
            problems,
            ToolCall::GetProblems {
                path: None,
                errors_only: false
            }
        );
    }

    #[test]
    fn every_listed_tool_parses() {
        let (_dir, root) = project();
        for tool in TOOLS {
            // Each fails cleanly or succeeds on empty arguments, but never panics or is unknown.
            if let Err(error) = call(tool.name, json!({}), &root) {
                assert!(!error.starts_with("Unknown tool"), "{}: {error}", tool.name);
            }
        }
    }
}
