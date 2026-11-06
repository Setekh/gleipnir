# Roadmap

Each item should be small enough to review alone, and has a check for when it's finished.
See [ARCHITECTURE.md](ARCHITECTURE.md) for the design.

```text
M0 groundwork -> M1 editor -> M2 workspace -+-> M3 search everywhere
                                            |
                                            +-> M4 packages + SDKs -+-> M5 code intelligence
                                                                    |
                                                                    +-> M6 debugger
```

M3 and M4 can run in parallel, as can M5 and M6. macOS is the focus for now, with `script/linux-check`
keeping the Linux build honest until it's picked up.

## M0: Groundwork (done)

Toolchain, pinned GPUI, a window on macOS, `text` and `syntax` crates.

## M1: Editor core

Goal: edit a real file with multiple cursors and save it, on macOS and Linux.

Status: built and passing on macOS. See the notes below the table for what's still open.

| # | Item | Finished when |
| --- | --- | --- |
| 1.1 | Linux build and CI: enable Wayland/X11 features, CI on macOS and Linux | CI builds and tests on both |
| 1.2 | `text`: selection sets, `edit_batch`, `transform` | Property tests: random multi-edits undo back to the original text |
| 1.3 | `ui`: theme, single-line input, virtual list | Input handles typing, selection and IME in a test window |
| 1.4 | `core`: `BufferStore`, load and save, external change reload | Two views on one path share one buffer; line endings survive a save |
| 1.5 | `editor`: rendering, scrolling, gutter, tabs, mouse selection | A 100k-line file scrolls without dropped frames; Odin/Go tabs align |
| 1.6 | Editing commands: motion, clipboard, undo, indent, brackets, comment toggle | Command tests pass for one cursor and for many |
| 1.7 | Multi-cursor: add above/below, next occurrence, column drag | Typing, paste and undo all work across cursors |
| 1.8 | `syntax`: incremental highlighting, extend selection | Typing in a 100k-line file stays under one frame (16 ms) |
| 1.9 | End-to-end driver | A script can type, assert state and take a screenshot |

Still open from M1:

- **Linux is deferred.** It builds and passes tests in an Arch container, but has never run on a
  real desktop. Hyprland, HiDPI scaling and the clipboard are all unknown there.
- **CI has never run,** because the repository is local. `script/check` covers the same ground.
- **The frame budgets are CPU-side measurements.** `script/e2e perf` times layout and painting, not
  GPU submission or what reaches the display.

## M2: Workspace shell

Goal: open a folder and work in it, with the layout from the reference screenshot.

Status: built and passing on macOS. See the notes below the table for what's still open.

| # | Item | Finished when |
| --- | --- | --- |
| 2.1 | Action registry and keymap, IntelliJ macOS shortcuts as defaults | Actions are listable and rebindable; Linux gets sensible modifier mapping |
| 2.2 | Window layout: tool rail, docks, resizing | Panels toggle and resize; layout restores on reopen |
| 2.3 | Split panes and tabs | Split either direction; the same file in two panes stays in sync |
| 2.4 | Project panel: lazy tree, keyboard nav, file icons | Opens a large repo instantly; follows external file changes |
| 2.5 | Project panel: file ops, reveal active file, gitignored and git status colours | Create, rename, delete and move all work, with confirmation for delete |
| 2.6 | Status bar and breadcrumbs | Shows position, encoding, line ending and indent; breadcrumbs are clickable |


## M3: Search Everywhere

Goal: double-shift finds any file, symbol or action.

Status: built and passing on macOS. On the Zed source tree, 4,395 files and 82,544 symbols index in
about four seconds in the background, and a search over 100,000 paths takes under 10 ms.

| # | Item | Finished when |
| --- | --- | --- |
| 3.1 | `index`: file scan respecting `.gitignore`, file watching | Handles a 100k-file repo without blocking the UI |
| 3.2 | Fuzzy matcher with camelCase and path-aware scoring | Ranking tests: `usrCtl` finds `UserController`, exact matches rank first |
| 3.3 | Popup, double-shift detector, tabs, provider framework | Detector tests: shift-shift opens it; shift-A-shift doesn't; a slow double tap doesn't |
| 3.4 | File and action providers, recents | Both searchable and runnable from the popup |
| 3.5 | Tree-sitter symbol index, all five languages | Finds symbols with no language server running |
| 3.6 | Direct entry points: go to file, symbol and action; recent files | Each opens the popup on the right tab |


## M4: Packages and SDKs

Goal: open a project on a clean machine and get working tools with minimal setup.

Status: built and passing on macOS. Every installable tool has been installed for real, with its
checksum verified: gopls, delve, ols, js-debug and typescript-language-server.

| # | Item | Finished when |
| --- | --- | --- |
| 4.1 | Manifest format, registry, five built-in packages | Language settings are read from manifests, not code; bad manifests give clear errors |
| 4.2 | `LanguageAdapter` trait | Implemented for all five |
| 4.3 | SDK registry and detection | Finds the toolchains on this machine, including `~/Odin` and nvm's Node, when launched without a shell `PATH` |
| 4.4 | Tool resolver | Tests for each level of the resolution order; the UI can report why a tool was chosen |
| 4.5 | Tool installer: pinned, checksummed, with confirmation | Installs gopls and a TS server; rejects a bad checksum |
| 4.6 | Settings UI: SDKs and packages | Add, remove and select SDKs per project; see and install tools |

Still open from M4:

- **Third-party packages** aren't loaded yet. The registry reads manifests like any other, so this is
  a matter of where to find them, which is stage 2 in the architecture.

## M5: Code intelligence

Goal: daily-driver code understanding. **After this, Gleipnir can be used to develop itself.**

Status: built and passing on macOS, against fakes in tests and the real servers end to end:
`E2E_TOOLS=<managed tools folder> script/e2e code` drives rust-analyzer, gopls,
typescript-language-server and ols through navigation, usages, completion, parameter info,
diagnostics, rename and reformat.

| # | Item | Finished when |
| --- | --- | --- |
| 5.1 | **Capability survey first:** each server against a sample project | A table of what each server really supports, which may change this plan |
| 5.2 | `lsp` client, tested with a fake server | Handles crashes, restarts, cancellation and server-to-client requests |
| 5.3 | `LanguageServers` hub, incremental document sync | After random edits, the server's view of the document matches ours |
| 5.4 | Diagnostics: underlines, gutter, Problems panel | Errors appear and clear as you type in all five languages |
| 5.5 | Go to definition and implementation, Cmd+click, chooser, back/forward | Works per the survey in every language |
| 5.6 | Find Usages panel, grouped by file with preview | Results update the editor on selection; Cmd+B on a definition shows usages |
| 5.7 | Completion, including auto-import and snippets | Accepting an item applies its additional edits as one undo step |
| 5.8 | Hover and signature help | Shows docs and the active parameter |
| 5.9 | File structure popup, LSP symbols in Search Everywhere | LSP results replace index entries without duplicates |
| 5.10 | Rename, code actions, format | Multi-file rename applies as one operation |

Shortcuts, IntelliJ's macOS ones: Cmd+Shift+F Find in Files, Cmd+B declaration or usages, Cmd+Alt+B implementations, Cmd+Shift+B
type declaration, Alt+F7 Find Usages, Cmd+Alt+F7 Show Usages, Cmd+F12 file structure, Cmd+6
Problems, F2 and Shift+F2 next and previous problem, Shift+F6 rename, Alt+Enter intentions,
Cmd+Alt+L reformat, Ctrl+Space completion, F1 quick documentation, Cmd+P parameter info, Cmd+[ and
Cmd+] back and forward.

Still open from M5:

- **rust-analyzer reports type errors on save**, from `cargo check`, as it does everywhere (this is
  how rust-analyzer works rather than something to fix); only its
  own diagnostics show while typing.

## M6: Debugger

Goal: IntelliJ-class debugging in all five languages.

Status: built and passing on macOS, against a fake adapter in tests and the real ones end to end:
`E2E_TOOLS=<managed tools folder> script/e2e debug` debugs Rust and Odin through lldb-dap, Go
through delve and TypeScript through js-debug, and the headless tests in `crates/dap/tests` do the
same per item 6.4 to 6.6, plus JavaScript and TypeScript compiled with source maps. What each adapter
needed is in [DEBUGGERS.md](DEBUGGERS.md).

| # | Item | Finished when |
| --- | --- | --- |
| 6.1 | `dap` client and session, tested with a fake adapter | Handles the full launch, stop, step and terminate cycle, and adapter crashes |
| 6.2 | Run targets, build step, run configuration switcher, gutter run icons | Finds and runs targets in each language |
| 6.3 | Breakpoints: gutter, persistence, follow edits | Survive restarts and stay on the right line through edits |
| 6.4 | Rust and Odin through lldb-dap | Headless test: stop at a breakpoint, read a local, step |
| 6.5 | Go through delve | Same test |
| 6.6 | JS and TS through js-debug, including child sessions | Same test, with source maps hitting TS lines |
| 6.7 | Debug panel: toolbar, threads and frames, lazy variables tree | Selecting a frame moves the editor and updates variables |
| 6.8 | Console, watches, evaluate | Output streams live; expressions evaluate in the selected frame |
| 6.9 | Inline values, conditional breakpoints, logpoints, run to cursor | Values appear inline at each stop; conditions are honoured |

Shortcuts, IntelliJ's macOS ones: Ctrl+R run and Ctrl+D debug the chosen configuration, Ctrl+Alt+R
and Ctrl+Alt+D choose one first, Ctrl+Shift+R and Ctrl+Shift+D run or debug the current file, Cmd+F2
stop, F8 step over, F7 step into, Shift+F8 step out, Alt+F9 run to cursor, Cmd+Alt+R resume, Alt+F8
evaluate, Cmd+F8 toggle a breakpoint, Cmd+Shift+F8 edit the breakpoint on this line or list them
all, Cmd+4 the Run panel and Cmd+5 the Debug panel. A click beside a line number toggles a
breakpoint, a right-click edits it, and the Run icon beside `main` runs or debugs.

Still open from M6:

- **Attaching** to a running process isn't offered; every session launches its program.
- **Run configurations can't be edited**: arguments, environment and working folder are the
  defaults each language's adapter gives. Tests aren't run targets yet.
- **Exception breakpoints** use each adapter's defaults; there's no UI to choose which.
- **Values can't be changed** while stopped (`setVariable`), and there's no memory or disassembly
  view.
- **Programs that read input** can't be typed to: the console takes expressions to evaluate, not
  the program's stdin, and runs are without a terminal.
- **Rust values look like C** under lldb, which has no Rust plugin; a formatter script like
  rust-lldb's would make strings and collections read naturally.
- **lldb-dap on Linux** is untested, as is all of Linux.

## M7: Closing the open items

Goal: everything left open by M1 to M6 that can be done on this machine. Linux on real hardware, CI
and GPU-side frame timing stay open above, as they need an Omarchy machine or a remote.

Done in the first batch:

- **Changes made outside the editor** reach everything that cares. Open files' folders are watched,
  so they reload at once wherever they are, with a 10-second poll left as a safety net. A reload
  replaces only the lines that changed, worked out by a line diff, so cursors, breakpoints and
  servers keep their places. Servers hear about files they don't have open
  (`didChangeWatchedFiles`), and breakpoints in closed files follow a diff of their old text.
- **Search Everywhere sees unsaved edits**: an open file's text is indexed again once typing
  pauses, and read from disk again if the edits are thrown away.
- **Symbols from servers** land on their name, whatever units the server counted the column in.
- **Find Usages previews** each result in the editor as you move through the list.
- **Rename and quick fixes across files undo in one step**, from any of the files, saving again
  the files that weren't open.
- **Tool updates**: when a manifest pins a newer version, Languages & SDKs offers to update, and
  installing it removes the old one.
- **JavaScript** is in the end-to-end `code` script.
- Already done, and only out of date here: the prompt when saving over a change on disk (M2), the
  panels beyond Project, symbols from servers in search (M5), and run targets that run (M6).

Done in the second batch:

- **The uses of the name under the cursor** are highlighted once it rests (`documentHighlight`).
- **Inlay hints** show types and parameter names in the text, in a quieter color. The editor's tab
  map became a general map between the file's columns and the screen's, so the cursor, clicks and
  selection step around hints as they do tabs. Hints move with edits and are asked for again
  when typing pauses. Servers that only give them when asked (gopls, tsserver, ols) are asked in
  their manifests.
- **Semantic highlighting** lays the server's colors over the syntax tree's where it knows more,
  such as which names are types, parameters or fields. Files over 20,000 lines keep the tree's.
- **Double-tap Option** (Ctrl on Linux), held on the second press, makes the arrows add carets.
- **Tabs drag** to reorder them, onto another pane's tabs or editor to move them there, and a pane
  emptied that way closes.
- **The project tree selects several entries**, with Cmd-click, Shift-click and Shift+arrows, and
  deletes or copies the paths of them all. **Typing in it jumps to a name**, with Up and Down for
  the next and previous match.
- **Find in Files** (Cmd+Shift+F) searches as you type, with case, whole-word and regex toggles, a
  file mask, unsaved edits searched as they stand, and a preview of the selected match. Enter opens
  it; Cmd+Enter moves the results to the Find panel.

Next, the debugger's: editable run configurations and tests as targets, attaching, exception
breakpoints, setting values, program input, readable Rust values; and third-party packages.

## Later

Find in Files, Git integration, semantic highlighting, test runner, terminal panel, third-party
packages (stage 2), WASM plugins (stage 3), Windows.
