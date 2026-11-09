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
   +-- project_panel   search   code_nav   debugger_ui   settings_ui
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
the title bar, and empty parts of the bar move or zoom the window. The branch's menu lists local
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

A commit is of the ticked files exactly, as IntelliJ's is: new ones are added, then `git commit --
<files>`, which leaves anything else staged out of it. Push sets the upstream on a branch's first
push; Update Project is `git pull`, as the person's git config has it.

### Project indentation

Indentation is the language's, then the person's Code Style settings, then the project's:
`.editorconfig` (every file from the one marked `root` down, nearest last, with its globs), and for
files Prettier formats, the nearest Prettier config, with its `overrides`. Prettier's wins where it
says something, and tabs or spaces it doesn't mention come from EditorConfig, as in Prettier.
JavaScript configs (`prettier.config.js` and the like) are run with Node, as Prettier does, and
remembered until they change; a project's are run in the background as it opens, so opening a file
doesn't wait. Changing any of these files re-indents the open files.

## Testing

| Layer | Approach |
| --- | --- |
| `text`, `syntax`, `index`, `packages` | Unit tests, property tests for edit and transform invariants |
| `lsp`, `dap` | Scripted fake servers for protocol and failure cases |
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
