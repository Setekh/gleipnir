# Architecture

Status: `text`, `syntax`, `index`, `core`, `ui`, `editor`, `workspace`, `project_panel`, `search`
and the `gleipnir` binary exist. The rest is the design to build towards.

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
   +-- project_panel   search_everywhere   code_nav_ui   debugger_ui   settings_ui
   |        (each feature contributes panels and actions to the workspace)
   |
workspace                window layout, docks, tool rail, split panes, tabs, action registry
   |
editor                   EditorView, rendering, input, multi-cursor commands
   |
ui                       theme, text input, virtual list, tree, popover, context menu
   |
core                     GPUI entities around the services below: BufferStore, LspHub, DebugHub
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

Detected candidates are stored in user settings, and each project picks one per language:

```toml
# ~/.config/gleipnir/sdks.toml
[[sdk]]
kind = "odin"
name = "Odin dev"
path = "~/Odin"

# <project>/.gleipnir/project.toml
[sdk]
go = "1.26"
odin = "Odin dev"
```

The SDK's environment is applied to everything spawned for that project: language servers, debuggers,
builds and run targets. That keeps a project's IDE features and builds on the same toolchain.

A tool resolves in this order, and the UI shows which one won and why:

1. An explicit path in settings
2. The project's SDK
3. A package-managed install, in `~/.local/share/gleipnir/packages`
4. `PATH`, including the login shell's `PATH`, captured once at startup

Downloads are pinned by version and verified by checksum per OS and architecture, and nothing is
installed without confirmation. A missing tool never blocks editing: the affected features are
disabled and the status bar offers to install it.

| Language | Server | Debugger | SDK |
| --- | --- | --- | --- |
| Rust | rust-analyzer | lldb-dap | rustup toolchain |
| Go | gopls | dlv | GOROOT |
| Odin | ols | lldb-dap | Odin install directory |
| TS / JS | a TS server, preferring the project's own `typescript` | js-debug | Node install |

## Code intelligence

`LspHub` runs one server per (language, project root) and owns document sync, using incremental
changes and UTF-16 positions. Results reach the UI as our own types, not `lsp-types`, so features
don't depend on the wire format.

Go-to behaviour:

- **Cmd+B / Cmd+click on a reference:** go to the definition.
- **The same on a definition:** show its usages, as IntelliJ does.
- **Several results:** show a chooser popup. **One result:** jump straight there.
- **Every jump:** recorded in a back/forward history shared across files.

Not every server supports every request. The UI checks advertised capabilities and disables what's
unavailable, instead of failing silently.

## Debugging

`dap` is a headless session state machine, driven the same way for every adapter and able to run
under test without a UI. `debugger_ui` renders that state.

- **Transports:** stdio and TCP.
- **Child sessions:** js-debug starts child sessions with a reverse `startDebugging` request, so a
  session is modelled as a tree of connections from the outset. Treating this as a special case later
  would be costly.
- **Breakpoints** belong to the project rather than a session, so they persist and follow edits
  through `transform`. They're re-sent at each session start.
- **Run targets** come from `LanguageAdapter::run_targets`, for example cargo binaries and tests, Go
  packages, an Odin package or the current JS/TS file. A target can have a build step that runs first.
- **Inline values** are built on the syntax crate: variable identifiers in the current function are
  matched with values from the stopped frame.

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
