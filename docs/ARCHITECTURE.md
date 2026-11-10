# Architecture

Status: `text`, `syntax`, `index`, `packages`, `core`, `ui`, `editor`, `workspace`,
`project_panel`, `search`, `settings_ui` and the `gleipnir` binary exist. The rest is the design to
build towards.

## Decisions

- **Licence:** MIT.
- **Platforms:** macOS is the focus for now. Linux comes next, targeting Omarchy (Arch with Hyprland),
  so Wayland on a tiling compositor is what will have to work. The code keeps building for it, and
  `script/linux-check` verifies that, but nothing has been run on a real Linux desktop yet.
- **Repository:** local only for now, so `script/check` stands in for CI.
- **Shortcuts:** IntelliJ's macOS and Linux keymaps are the defaults, rebindable in
  `~/.config/gleipnir/keymap.conf`. Following IntelliJ, `Cmd+O` and `Cmd+Shift+O` are Go to Class and
  Go to File, so opening a file or folder is in the File menu, with no shortcut.

## Goals

An IDE for TypeScript, JavaScript, Odin, Rust and Go, on macOS and Linux, with:

- a Project tree (left panel) in a dockable layout, with split editor panes
- multi-cursor editing
- Search Everywhere on double-shift: files, symbols and actions
- code intelligence: completion, hover, diagnostics, go to definition/implementation, find usages
- IntelliJ-style debugging: frames, a variables tree, watches, inline values, conditional breakpoints
- a package system for language support, and configurable SDKs

Inspired by IntelliJ, not a clone: familiar workflows and shortcuts, with our own look.

## Constraints that shaped this design

These were checked against the pinned Zed commit.

1. **GPUI only provides primitives.** It has `div`, `list`, `uniform_list`, `canvas`, `anchored`,
   `deferred`, `img`, `svg`, plus text shaping and input handling. Docks, panes, trees, tabs and the
   editor itself are all ours to build.
2. **Zed's higher-level crates are GPL-3.** `editor`, `workspace`, `project`, `language`, `lsp`,
   `dap`, `text`, `rope`, `fuzzy`, `project_panel` and `extension` are GPL-3.0-or-later. GPUI and the
   crates it pulls in are permissive; a licence audit of our dependency graph found no crate that
   requires the GPL. **Do not copy from or depend on Zed's GPL crates** unless Gleipnir itself
   becomes GPL. GPUI's own examples are Apache-2.0 and fine to learn from.
3. **Linux renders through wgpu with Wayland and X11 backends**, and needs system packages (Vulkan,
   xkbcommon, wayland, fontconfig and others). The features must be enabled explicitly, because our
   pin currently enables only `font-kit` and `runtime_shaders`.
4. **Hyprland draws no title bars and reserves Super.** The app requests server-side decorations,
   which on Hyprland means none, and sets an `app_id` so window rules can target it. No default key
   binding uses Super. Compositors that require client-side decorations, such as GNOME, will need a
   drawn title bar, which isn't built.
5. **Apps launched from a desktop launcher don't inherit the shell's `PATH`.** This machine shows why
   it matters: `odin` is in `~/Odin` and `node` is under nvm. Tool discovery can't rely on `PATH`.

## Crates

Dependencies point downward only. Everything above the line is free of GPUI, so it compiles quickly
and can be tested without a window.

```text
gleipnir                 binary: startup, wiring, keymap defaults
   |
   +-- project_panel   search   code_nav   debugger_ui   settings_ui   git_ui   agent
   |        (each feature contributes panels and actions to the workspace)
   |
workspace                window layout, docks, tool rail, split panes, tabs, action registry
   |
editor                   EditorView, rendering, input, multi-cursor commands
   |
ui                       theme, text input, virtual list, tree, popover, context menu
   |
core                     GPUI entities around the services below: BufferStore, LanguageServers, Debugger
- - - - - - - - - - - - - - - - - - - - - - - - - - - - - - - - - - - - - - - - - - - - -
packages    manifests, package registry, SDK registry, tool resolution and installation
lsp         JSON-RPC transport and typed LSP client
dap         DAP transport, session state machine, child sessions
index       file scanning, fuzzy matching, tree-sitter symbol index
syntax      languages, grammars, highlighting, outlines, structural selection
text        rope buffer, undo, coordinate conversions, selection transforms
```

Services run on their own threads and communicate over channels, with no dependency on GPUI's
executor. `core` bridges them into GPUI entities. That lets `lsp` and `dap` be tested against fake
and real servers with no UI.

## Editor model: built for multi-cursor and splits

Both are core requirements, so they shape the model from the start.

```text
BufferStore  --(one per path)-->  Buffer  <--(many)--  EditorView
                                  text, undo,          selections, scroll,
                                  syntax tree,         fold state, pane
                                  LSP doc version
```

- **One `Buffer` per file, many `EditorView`s.** Two splits on one file share text, undo, syntax
  tree and LSP state. Only selections and scroll position belong to the view.
- **A tab is an editor or an image.** A pane's items are `PaneItem`s: an `EditorView`, or an
  `ImageView` for files GPUI can draw, SVG included, which has no buffer, so it's never unsaved.
  `open_path` routes images to the preview and then fails with `ShownAsImage`, as there's no
  editor to give back; `open_as_text` opens any file in an editor, as an SVG's "Edit as Text" does.
- **Selections are a sorted, non-overlapping set**, always at least one. A single cursor is just a
  set of one, so there is only one code path.
- **Multi-cursor edits are atomic.** `Buffer::edit_batch(Vec<(Range, String)>)` applies all edits
  as one change and one undo step. Downstream, tree-sitter and LSP receive them as ordered edits.
- **Everything anchored to text follows edits through one function**, `transform(offset, edit, bias)`.
  This covers other views' selections, breakpoints, bookmarks, diagnostics and inline values.
- **Undo history lives on the buffer.** A transaction records the selections around it. The view
  that presses undo restores them, which jumps it to the change, and other views transform theirs.
- **Commands are pure functions.** Everything that edits text is in `text::ops`, over a buffer and a
  selection set. The view only maps input to commands, so each is testable without a window.

### Syntax and threading

Parsing never runs on the UI thread. An edit updates the existing tree's positions in microseconds
and a `ParseJob` reparses in the background. Until it finishes, highlighting uses the adjusted tree,
which is right except around the edit itself. At most one job runs per buffer, so a burst of typing
collapses into few parses.

The grammars' highlight queries need care, as they were written for different editors:

- **Precedence differs.** Go expects the first matching pattern to win on a node, the others the
  last. See `first_pattern_wins`.
- **Odin's query uses Neovim's `#lua-match?`.** tree-sitter doesn't evaluate that, so without our
  handling every identifier matches those patterns.
- **Rust's query has an upstream bug** in its constant pattern, corrected by an override query.

## Search Everywhere

One double-shift popup over pluggable providers, each returning scored results as they arrive:

| Provider | Source | Available |
| --- | --- | --- |
| Files | `index` file scanner, respects `.gitignore` | immediately |
| Actions | workspace action registry | immediately |
| Symbols | our tree-sitter index, then merged with LSP `workspace/symbol` | as soon as files are scanned, better once servers are ready |
| Recents | recently opened files and used actions, boosted in ranking | immediately |

The tree-sitter index is why symbol search doesn't depend on a language server finishing its
indexing, which matters for slow servers and any that have weak workspace symbol support. LSP results
replace index entries for the same location, as they're more precise.

Double-shift detection is a small state machine over GPUI's modifier events: shift down and up, then
down again within a time window, with no other key in between.

`core::Project` owns everything the search reads: one file watcher per project, the file list, the
symbol index and git status. The project tree reads git status and change events from it too, so a
folder is only ever watched and scanned once. Changes inside ignored top-level folders, like
`target/`, don't trigger git refreshes, as builds churn through them constantly.

Matching is our own, in `index::fuzzy`: a dynamic program that rewards word starts, camelCase humps
and consecutive runs, so `usrCtl` finds `UserController`. Searches run on every core and keep only the
best results, and a newer keystroke cancels an older search still running.

## Packages and SDKs

Three separate concepts that are easy to blur together:

| Concept | What it is | Example | Who manages it |
| --- | --- | --- | --- |
| **Package** | A bundle describing support for a language | "Go support" | Gleipnir |
| **Tool** | An executable a package needs | `gopls`, `dlv` | Gleipnir installs it, or you point at one |
| **SDK** | The toolchain your project builds with | Go 1.26 at `/opt/homebrew/opt/go` | You choose it, Gleipnir detects candidates |

### Packages

A package is a directory with a manifest. The five built-in languages use this same format, embedded
in the binary, so the system is exercised from day one rather than added on later.

```toml
# packages/go/package.toml
[package]
id = "go"
version = "0.1.0"

[language]
name = "Go"
extensions = ["go"]
comment = "//"
hard_tabs = true
grammar = "builtin:go"

[server.gopls]
tool = "gopls"
args = ["serve"]

[debugger.delve]
tool = "dlv"
args = ["dap", "--listen=127.0.0.1:{port}"]
transport = "tcp"

[tool.gopls]
version = "..."
install = { kind = "go-install", module = "golang.org/x/tools/gopls" }

[sdk]
kind = "go"
detect = ["env:GOROOT", "path:go", "dir:/usr/local/go", "glob:~/sdk/go*"]
env = { GOROOT = "{sdk}", PATH = "{sdk}/bin:{PATH}" }
```

Some behaviour can't be declared, such as reading `cargo metadata` to find run targets. That goes
behind one Rust trait, which manifests and built-ins both implement:

```rust
trait LanguageAdapter {
    fn server_command(&self, ctx: &ProjectContext) -> Result<Command>;
    fn run_targets(&self, ctx: &ProjectContext) -> Result<Vec<RunTarget>>;
    fn debug_launch(&self, target: &RunTarget, ctx: &ProjectContext) -> Result<DebugLaunch>;
}
```

Rollout is staged so we only pay for what's needed:

1. **Declarative packages, built in.** Part of this roadmap.
2. **Third-party declarative packages** installed from a folder or URL, with grammars compiled to
   WASM. tree-sitter's `wasm` feature supports loading them without running native code.
3. **WASM code plugins** implementing `LanguageAdapter`, sandboxed with wasmtime. Not on this
   roadmap, and only worth doing when a real package can't be declared. UI plugins are out of scope.

Native dynamic libraries are ruled out: Rust has no stable ABI, and they'd run unsandboxed.

Note for stage 2: tree-sitter 0.25's `wasm` feature brings wasmtime-c-api 29. A stage 3 plugin host
would need a wasmtime version compatible with that, or a tree-sitter upgrade.

### SDKs

SDKs added by hand, and tools pointed at by hand, are remembered for every project in the state
folder's `toolchains.json`. Each project's choice of SDK is part of its saved layout, outside the
project like the rest. With no choice made, the default follows the user's shell: an SDK named by an
environment variable like `GOROOT`, then the one on `PATH`, then the newest installed.

The SDK's environment is applied to everything spawned for that project: language servers, debuggers,
builds and run targets. That keeps a project's IDE features and builds on the same toolchain.

A tool resolves in this order, and the UI shows which one won and why:

1. An explicit path in settings
2. The project's own copy, like `node_modules/.bin`, which matches the project's versions
3. The project's SDK
4. A Gleipnir-managed install, in the state folder's `tools/<name>/<version>`
5. A fixed system location, like `lldb-dap` inside Xcode
6. `PATH`, including the login shell's `PATH`, captured once at startup

The built-in manifests live in `crates/packages/builtin`. Each pins its tools' versions:
TypeScript is held at 6, as TypeScript 7 is the native rewrite and has no `tsserver` for
typescript-language-server to drive.

Downloads are pinned by version and verified by checksum per OS and architecture. A tool that can
be installed is installed when it's first needed, as IntelliJ bundles its own: a language server
when a file in its language opens, a debugger when debugging starts. The status bar or the console
says so meanwhile. A failed install isn't retried by itself; Settings shows why and installs it again
on request. `GLEIPNIR_AUTO_INSTALL=0` turns this off, as the plain e2e scripts do. A missing tool never
blocks editing: the affected features are disabled until it's there.

| Language | Server | Debugger | SDK |
| --- | --- | --- | --- |
| Rust | rust-analyzer | lldb-dap | rustup toolchain |
| Go | gopls | dlv | GOROOT |
| Odin | ols | lldb-dap | Odin install directory |
| TS / JS | a TS server, preferring the project's own `typescript` | js-debug | Node install |

## Code intelligence

`LanguageServers` (in `core`, owned by the workspace) runs one server per package and project,
starting it when a file of its language is first registered. It owns document sync: incremental
changes from each edit's UTF-16 ranges, or the whole text for servers that ask for that. Edits reach
it through buffer events, which arrive after the update that made them, so every request first
catches its document up: if the server's copy is behind, the whole text goes first, and the edit
events it already covered are skipped by version. That's what lets the editor ask for completion in
the same keystroke that typed the character.

A server is *busy* while it reports `$/progress`, and, for servers whose manifest says
`reports_ready` (rust-analyzer), until it says it's quiescent. Without that, the gaps between its
loading steps look like readiness, and early requests get empty answers. When an answer is empty
while the server is busy, the status bar says so instead of "not found". Servers configured through
`initializationOptions` get them from their manifest; TypeScript uses that to keep tsserver's
syntax-only server from answering navigation before the project has loaded.

The editor knows nothing of LSP. It asks a `SemanticsProvider` for completions, documentation,
parameter info and trigger characters, in its own types, and emits `GoToDefinition` on Cmd+click.
`code_nav` implements the provider over `LanguageServers`, and adds everything else through
workspace hooks: an editor hook to attach the provider and register each buffer, actions, the Find
and Problems panels, the status bar item, and a picker popup for choices.

Go-to behaviour:

- **Cmd+B / Cmd+click on a reference:** go to the declaration.
- **The same on a declaration:** show its usages, as IntelliJ does.
- **Several results:** a chooser popup. **One result:** jump straight there.
- **Every jump:** recorded in the workspace's back/forward history (Cmd+[ and Cmd+]).

Edits from rename and quick fixes arrive as a `WorkspaceEdit`. Files open in the editor are edited
there and left for the user to save, as with their own typing. Files that weren't open are opened,
edited and saved, as nothing would show they'd changed otherwise.

Not every server supports every request. Actions check advertised capabilities first and say when
a server can't do something, such as implementations under ols.

## Debugging

`dap` is headless: a client that talks to adapters over stdio or TCP, the launch handshake, typed
requests, and a state machine for a session's events. It runs the same under test as in the app,
and `crates/dap/tests/adapters.rs` drives the real adapters with it, building a small program in
each language, stopping at a breakpoint, reading a local and stepping.

`Debugger` (in `core`, owned by the Debug panel) holds what the window runs:

- **Run targets** come from each language's adapter in `packages`: Cargo binaries, Go main
  packages, Odin packages with a `main` procedure, package.json scripts, and any JS or TS file
  directly. A target's `source` is its entry file or package folder, which maps a file to what
  runs it, and puts a Run icon beside `main`.
- **A launch** is one press of Run or Debug: a build if the target has one, then the program, on
  its own or under an adapter. Its console collects the build's output, the program's, and the
  debugger's messages. Running the same target again replaces its launch, stopping the old one.
  Builds and plain runs start in their own process group, so stopping `go run` also stops the
  program it started.
- **Sessions** form a tree. js-debug answers the first launch by asking, with `startDebugging`, for
  a second connection that debugs the actual program; that child does the stopping and stepping.
  Whichever session stopped last has the focus: stepping, frames, variables and watches refer to it.
- **Breakpoints** belong to the project, not a session. They're saved with the layout, sent at
  each session's start and again whenever they change, and move with their line as the file is
  edited: lines added or removed above shift them, and Enter before a line's text takes the
  breakpoint down with it. A status per breakpoint records whether the running sessions could
  place it.

The handshake orders itself around the adapters' differences: lldb-dap and js-debug answer `launch`
only after `configurationDone`, while delve answers first and sends `initialized` after. So the
launch is sent without waiting, breakpoints go once `initialized` arrives, and the launch is
awaited last.

The editor knows nothing of debugging. It asks a `DecorationProvider` each frame for breakpoints,
the execution line, Run icons and values to show beside lines, and reports gutter clicks as events.
`debugger_ui` implements the provider: values come from the stopped frame's local scopes, matched
to the names the syntax tree finds between the start of the running function and the stop.

What each adapter needed, found by running them for real, is in
[DEBUGGERS.md](DEBUGGERS.md).

### Test runner

A test target is run like any other, and its results are read from what it prints, so they fill
in live and work the same under the debugger. `TestRun` (in `core`) follows one of four formats,
chosen by the target: libtest's text for Rust, `go test -json` (and `-test.v` text under delve,
which can't pass `-json` through), and Odin's runner log. It builds a tree of suites (a test
binary, a Go or Odin package), groups (Rust modules) and tests (Go subtests nest under theirs), with
counts kept per node so a run of thousands updates in place. Each test collects its own output.

Two streams arriving separately shaped the parsers. Cargo names each test binary on stderr while
the binary announces itself on stdout, so the two are paired up in order rather than read as they
come. Odin logs a test's state from its main thread after the fact while the test logs from its
own, so a failure is matched to its test by the procedure it names, with the summary at the end as
a fallback.

What only a language knows stays in its adapter in `packages`: narrowing a target to some of its
tests (Cargo's `--lib`/`--test` plus `--exact` names, Go's `-run` patterns, Odin's
`ODIN_TEST_NAMES`), which Rerun Failed and the tree's Run and Debug items use, and finding where
each test is declared, by following a Rust test's module path from the crate root through `mod`
declarations, or finding the function in a Go or Odin package. Located tests give the gutter's
Run icons their last result.

JavaScript and TypeScript tests run with jest or vitest, whichever the nearest config (or failing
that, package.json) above the test file names, from that folder and with the copy installed nearest
to it, as a monorepo installs once at the top. A file's `describe`, `it` and `test` calls are found
by a scan that skips strings and comments and matches parentheses, so each gets a Run icon and runs
alone through `-t` with its full title. These runners print results only as text for people, so the
tree comes from the JSON report they write when the run ends (`--json --outputFile`, or vitest's
JSON reporter), while the console shows their usual output. Under the debugger, Node runs the
runner's own script in one process, so breakpoints in tests bind through the source maps babel or
ts-jest make. A title can hold any character, so inside the tree a test's titles are joined by an
invisible separator, and its `-t` pattern by spaces as the runners join them.

### Completion while typing

Typing a word asks for completions, and typing more of the same word while that list is on its way
waits for it and filters it, rather than asking again on every key: a slow server would otherwise
fall behind and show nothing until typing stopped. tsserver loads a project, and its auto-imports,
on its first completion, which takes seconds in a monorepo, so servers marked `warm_up` are asked
once as the first file opens and show as loading the project until they answer.

### Main toolbar

The OS title bar is replaced by IntelliJ's main toolbar, drawn by the workspace: the project, whose
menu lists recent projects; the Git branch, read from `.git/HEAD` and polled; and items features
add, ordered by a number: the run configuration with Run, Debug and Stop, Search Everywhere and
Settings. On macOS the window's buttons sit over its left end, gpui is told the app owns dragging
the title bar, and empty parts of the bar move or zoom the window. On Linux the window asks for
client-side decorations and the bar draws minimise, maximise and close itself, at the ends and in
the order the desktop's `button-layout` gives, leaving out what the compositor can't do; right
clicking it opens the compositor's window menu. It draws them whatever the compositor answers, as
Hyprland answers that it will decorate the window and then draws no title bar. With no system menu
bar there, the bar also shows the app's menus, read back from `cx.get_menus()` so they're defined
once in `main.rs`, where macOS's app menu folds into File; an item dispatches its action to
whatever had focus before the menu opened. A window that isn't tiled gets a border, a shadow
and edges that resize it (`workspace/decorations.rs`). `GLEIPNIR_WINDOW_DECORATIONS=server` asks
for the compositor's title bar instead. The branch's menu lists local
branches, then remote ones, most recently committed to first, and switches with `git switch`,
which refuses rather than lose uncommitted work; its message is shown as it is.

### Settings

`Settings` (in `packages`, as plain data) is read from `settings.json` with a default for every
field, so a partial or older file reads fine. `core` holds it as a global; changing it saves it and
notifies observers, which is how everything follows at once: the app rebuilds the `Theme` (dark or
light, with the font sizes) and re-applies indentation to open files, toolchains pick up whether to
install tools, and the editor reads its font, completion, Parameter Info and inlay hint settings
as it draws. The Settings screen lists each setting as data, with a label, a description and a
control (a switch, a stepper or a choice), which is what its search runs over. Shortcuts added on
its Keymap page are appended to `keymap.conf` and bound at once; removing one rebinds every key from
the defaults, as gpui can't take a binding back alone.

`settings.json` and the themes folder are looked at once a second (a few file times), and read again
when they change, so they can be changed by hand or by an AI agent and the window follows.

### Themes

A theme is a TOML file: a `name`, the built-in theme it starts from (`appearance`, dark or light),
and `[colors]` and `[syntax]` tables of `#rrggbb` or `#rrggbbaa` colors. The built-in themes are
themselves such files (`crates/ui/themes`), setting every key, so the format is the one source of
truth; the keys a file may set come from one list in `theme.rs`, which also writes the README that
Gleipnir puts in the themes folder alongside full examples, so instructions can't drift from the
code. A file with a mistake is left out, with the key and value that's wrong, shown in Settings.

### Git

Git is run, not reimplemented, so every repository format and setting works as it does in a
terminal; what git says when it refuses is what's shown. `index::git_ops` holds the commands, each
tested against a real repository, and `git_ui` the Commit window, the log and the diff view.

An open file keeps its committed text (`git show HEAD:path`), read again after a save and whenever
the project's git status changes, and compares its live text with it off the UI thread a moment
after typing stops (`similar`'s line diff), so the gutter's change marks include unsaved edits.
Rollback Lines puts a change's committed lines back as one undoable edit.

Diffs and merges take their positions from the lengths of the diff's runs rather than from
`similar`'s own indices, which around a deletion aren't always where the run is (a property test
found this). A conflicted file's three versions come from git's index (`:1:`, `:2:`, `:3:`), and
`merge3` merges them line by line as git does: what one side changed is taken, what both changed
the same is taken once, and changes with nothing unchanged between them are a conflict.

A commit is of the ticked files exactly, as IntelliJ's is: new ones are added, then `git commit --
<files>`, which leaves anything else staged out of it. Push sets the upstream on a branch's first
push; Update Project is `git pull`, as the person's git config has it.

The log's graph (`git_ui::graph`) is worked out from each commit's parents, in the order the log
lists them (`--topo-order`, children first). A lane waits for the commit named next in it; a
commit takes the lane waiting for it, or the leftmost free one, and its first parent carries on
there. A merge's other parents join a lane already waiting for them or start one, and several lanes
waiting for one commit all end at its dot, which is how a fork shows. Lanes that end leave gaps the
lanes to their right close, leaning over in that row. Each row keeps its lines from top to dot to
bottom edge, so the log draws only the rows on screen, in a virtual list.

Blame is `git blame --porcelain --contents -` with the editor's text on its input, so it describes
what's on screen, unsaved edits included; whole messages come from one `git show` for all the
commits. The editor has a general place for text beside lines (`GutterText`) and knows nothing of
git: it moves the labels with each edit, blanks lines that change, shows a label's details on
hover, and reports clicks. Git's side blames again a moment after typing stops, after a save and
after a commit, and drops an answer about text that has changed since.

Interactive rebase never opens an editor. The window builds the whole plan, and git is started
with `GIT_SEQUENCE_EDITOR` set to copy our todo list over its own and `GIT_EDITOR=true`. New
messages are written to files in the repository's git folder and applied by an `exec git commit
--amend -F <file>` line after the commit they belong to; a squash is a `fixup` followed by such a
line with the messages joined. So a rebase that stops at a conflict, and is continued later, even
after a restart, still gets every message right. `rebase.missingCommitsCheck=error` and a check that
HEAD hasn't moved since the window opened keep a commit from being lost. A stop lands in the
Commit window like any conflict, with Continue (`--continue`, keeping git's messages) and Abort, as
merges and cherry-picks that stop do too.

Git runs with `GIT_OPTIONAL_LOCKS=0`, so reading the state, as `git status` does every time
something changes, never holds the index lock a commit or rebase is about to need.

### Inline suggestions

The editor asks its suggester after each character typed or deleted at the end of a line, blank
or not, shows the answer in grey after the cursor with its score, takes it with Tab as one undo
step, or its next word or line, leaving the rest showing, and drops it when the cursor moves;
typing what it says keeps the rest. A suggestion of several lines gets rows of its own under its
line: the layout maps lines to rows (`Rows` in `element.rs`) rather than assuming one each, so
clicks, the gutter and scrolling all account for them. Two suggesters stand behind it. The file's own
looks for the end of what's been typed elsewhere in the file and offers what most often follows it
there, or the punctuation the sibling lines end with, scored by the share of the evidence that
agrees; it's instant and needs nothing. The model's is llama.cpp's server running a small
fill-in-the-middle code model (Qwen2.5-Coder), given the code before and after the cursor and asked
for the rest of the line. On a blank line, or after one ending in `{`, `(`, `[` or `:`, it may
write more lines: statements as deep as the blank line, or a block's body and its closer, cut at
the first line that leaves them, and then to the lines it's at least the minimum score sure of all
together. It's also shown, through llama.cpp's `input_extra`, the piece of each recently used open
file that shares the most names with the code around the cursor (`related.rs`, 6 KB at most), so a
function defined in another file is called with its real arguments. A score is the probability of
that exact text, the product of the odds the model gave each token. The file's answer shows at once when it's sure; otherwise the
likelier of the two does.

llama.cpp and the model are installed like any tool, pinned and checksummed (the model's download
streams to disk and is hashed as it comes), when first wanted and only where installing is allowed.
One server serves every window, on 127.0.0.1 and a free port; it's stopped at exit however
Gleipnir ends, and one left by a crash is stopped on the next start, by the process id kept in the
state folder.

### Terminal

`crates/terminal` runs each tab's program on a pseudo-terminal with alacritty_terminal
(Apache-2.0), whose reader thread parses the output into a grid; the session hears of new output
over a channel and redraws once per batch. The view draws the grid itself, in the editor's font:
runs of cells styled alike are shaped together and placed at their columns, a wide character on
its own so the rest stay aligned, with backgrounds, the selection and the cursor as quads. Keys go
to the program as xterm sends them (`keys.rs`), after the IDE's own bindings: those the shell
should have, like Ctrl with a letter, Tab and the arrows, are bound to `NoAction` in the
`Terminal` context so the IDE's bindings for them stand aside. Colors are the theme's 16, xterm's
cube and grays for the rest, and any a program sets. A shell's tab closes when it exits.

Coding agents are tabs too (`agents.rs`). An agent is started through the person's login shell,
interactively, as `$SHELL -l -i -c 'claude "$@"'`, so their `PATH` and any function wrapping the
command apply, such as one choosing an account per folder; anything the IDE adds is passed after
it, untouched. Other crates add to every agent's start through `add_agent_setup`: the agent
crate's `launch.rs` gives Claude Code the window's MCP server with `--mcp-config` and a plugin
holding the Gleipnir skill with `--plugin-dir` (written to the state folder, loaded for that
session only) and a line saying it's in Gleipnir with `--append-system-prompt`, and Codex the
server as `-c mcp_servers.gleipnir.*` overrides and the skill's text as that session's
`developer_instructions`, after any the person has set themselves. The agent's own
configuration isn't touched. Gleipnir never installs an agent, signs one in or sits between it
and its service. Agents' tabs are in their own tool window on the right, Agents (the same tab
strip, as `AgentsPanel` around a `TerminalPanel`), which offers the agents when it's empty rather
than starting one.

Using Claude Code this way is what Anthropic documents: its legal and compliance page allows "an
end user … signing in to the unmodified Claude Code binary with their own Claude subscription",
and its VS Code page says to "run `claude` in the editor's integrated terminal" where there's no
extension. So that it stays that way, Gleipnir doesn't cross these lines, for any agent:

- It never reads, stores, copies or forwards an agent's credentials or tokens, and offers no
  sign-in of its own; the agent signs in through its maker's own flow.
- It never types a prompt or presses Enter for the person, and never runs an agent headless
  (`claude -p` and the like). Send to Agent types a reference and stops.
- It never proxies an agent's traffic or points it elsewhere (`ANTHROPIC_BASE_URL` and the like),
  and the MCP server never calls a model service.
- It doesn't modify, bundle or install an agent, and uses agents' names only as plain text, with
  icons of its own.

### Project indentation

Indentation is the language's, then the person's Code Style settings, then the project's:
`.editorconfig` (every file from the one marked `root` down, nearest last, with its globs), and for
files Prettier formats, the nearest Prettier config, with its `overrides`. Prettier's wins where it
says something, and tabs or spaces it doesn't mention come from EditorConfig, as in Prettier.
JavaScript configs (`prettier.config.js` and the like) are run with Node, as Prettier does, and
remembered until they change; a project's are run in the background as it opens, so opening a file
doesn't wait. Changing any of these files re-indents the open files.

## Coding agents

Agents like Claude Code and Codex are the person's own programs, signed in by them and run in
Gleipnir's terminal. Gleipnir offers them tools through MCP and nothing more: it never sees their
credentials or their traffic. The `agent` crate is the MCP server.

- **One socket per window.** Each window with a project listens on a Unix socket in
  `/tmp/gleipnir-<uid>`, a folder only its owner can enter (0700, checked to be ours and not a
  link), the socket itself 0600. The folder is short on purpose: a socket's path can't be longer
  than 104 bytes on macOS, and the temporary and state folders can be. A socket is named
  `<pid>-<n>.sock` and removed when its window closes and when Gleipnir exits; ones a crash leaves
  are removed at the next start, once their process is gone.
- **`gleipnir mcp` is only a pipe.** An agent starts it as a stdio MCP server; it connects to the
  socket named by `GLEIPNIR_SOCKET` and copies bytes both ways. It's handled at the top of `main`,
  before GPUI, so it starts at once. `mcp_config_json` writes the configuration Claude Code's
  `--mcp-config` takes, and `socket_path` gives a window's socket to whatever starts agents.
- **The socket speaks MCP itself**: JSON-RPC 2.0, one message per line, as MCP's stdio transport
  does. It's written by hand over serde_json (`rpc.rs`): the part of MCP that tools need is small,
  and the official Rust SDK would bring tokio and much more besides. Each connection has a thread
  reading and one writing. `initialize`, `ping`, `tools/list` and malformed messages are answered
  there; tool calls go over a channel to the window, where each runs as a task of its own, so a
  slow one, like a review, holds up no other, and answers go back as they finish. Arguments are
  checked (`tools.rs`) before the workspace is touched, and a tool that fails answers with
  `isError` and why, which the agent reads and can act on.
- **Tools work on what the window has.** Paths are from the project root, and anything that
  resolves outside it, by `..` or through a link, is refused. Lines and columns count from 1,
  columns in characters. Reading takes an open file's buffer, with its unsaved edits, or else the
  disk. `edit_file` makes its replacement in the buffer with `apply_external_edits`, one undo step,
  and leaves it unsaved; a file that isn't open gets a tab beside the active one, without taking
  focus (`Workspace::open_in_background`), as an unsaved change has to be somewhere the person can
  see, save or undo it. Lookups ask the language servers, by position, or by a name found first in
  the project's own symbol index. Tests run through the debugger as the gutter's Run icons run
  them, and their results come from the same `TestRun` the Run panel shows.
- **Proposed edits are reviewed in the diff view.** `propose_edit` opens git_ui's `DiffView` in
  review mode: the file as it is on the left, the proposal on the right, ✓ and ✕ on each change,
  and Accept All and Reject All. Accepting a change edits the buffer as an undoable step. If the
  file changed meanwhile, as from another agent's edit, nothing is taken from the stale diff: the
  proposal is carried over to the file as it is now with `merge3`, as git would, so the other
  edit isn't undone, and changes that clash with it are left out. Proposals wait
  their turn while another popup is open. The call waits for the person's decision, but only 50
  seconds by default, and then answers "pending" with an id that `get_proposal` waits on again:
  agents give up on a tool call after a while (Codex after a minute, unless configured), and an
  answer sent after that would be lost, while asking again costs nothing.

## Testing

| Layer | Approach |
| --- | --- |
| `text`, `syntax`, `index`, `packages` | Unit tests, property tests for edit and transform invariants |
| `lsp`, `dap` | Scripted fake servers for protocol and failure cases |
| MCP server | Our own client: the protocol line by line, tool calls against a real workspace, the socket with several clients at once, and the built `gleipnir mcp` as a subprocess. Never a real agent, which would spend the person's account |
| Real servers and adapters | Integration tests against sample projects per language, reporting a skip when a tool is missing |
| UI logic | GPUI's test support, simulating keystrokes without a window |
| End to end | `script/e2e`: a scripted driver on the real app sends keys, asserts on state and takes screenshots. See `crates/gleipnir/src/driver.rs` |

Linux is checked continuously in CI from the first editor milestone, not ported at the end.

## Known risks

- **Server capability gaps are unmeasured.** No server has been run through Gleipnir yet. Odin has no
  interfaces, so "go to implementation" may mean nothing under ols. The roadmap starts its LSP work by
  measuring this.
- **js-debug** is the most complicated adapter, because of child sessions and because it's distributed
  as a Node program.
- **`lldb-dap` on Linux** depends on the distro's LLVM packaging.
- **Linux is untested on real hardware.** `script/linux-check` covers building and tests, but only
  running it on Omarchy shows how input, HiDPI and rendering behave under Hyprland.
- **GPUI is pre-1.0** and changes often. The commit pin limits this, but upgrades will need work.
- **Editor performance on big files** is measured by `script/e2e perf`, but only the CPU side of a
  frame, not GPU submission or what reaches the display.
