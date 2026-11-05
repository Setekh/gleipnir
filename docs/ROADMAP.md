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
- **IntelliJ's double-tap Option/Ctrl to clone a caret** uses Cmd+Alt+arrow or Ctrl+Alt+arrow for
  now. It can share the gesture detector from item 3.3.
- **Saving over an external change doesn't prompt.** The status bar flags the conflict, and saving
  overwrites the file. A proper dialog fits better with the workspace in M2.

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

Still open from M2:

- **Tabs can't be dragged** to reorder them or move them between panes. Splitting copies the
  current tab, and closing removes it, which covers moving one by hand.
- **The tree selects one entry at a time.** Multi-select, and typing to jump to a name as IntelliJ
  does, aren't built.
- **Only the Project panel exists,** so the right and bottom docks are tested with stand-in panels.
- **Open files outside the project** are still polled every second for external changes. Files
  inside it are watched.

## M3: Search Everywhere

Goal: double-shift finds any file, symbol or action.

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

| # | Item | Finished when |
| --- | --- | --- |
| 4.1 | Manifest format, registry, five built-in packages | Language settings are read from manifests, not code; bad manifests give clear errors |
| 4.2 | `LanguageAdapter` trait | Implemented for all five |
| 4.3 | SDK registry and detection | Finds the toolchains on this machine, including `~/Odin` and nvm's Node, when launched without a shell `PATH` |
| 4.4 | Tool resolver | Tests for each level of the resolution order; the UI can report why a tool was chosen |
| 4.5 | Tool installer: pinned, checksummed, with confirmation | Installs gopls and a TS server; rejects a bad checksum |
| 4.6 | Settings UI: SDKs and packages | Add, remove and select SDKs per project; see and install tools |

## M5: Code intelligence

Goal: daily-driver code understanding. **After this, Gleipnir can be used to develop itself.**

| # | Item | Finished when |
| --- | --- | --- |
| 5.1 | **Capability survey first:** each server against a sample project | A table of what each server really supports, which may change this plan |
| 5.2 | `lsp` client, tested with a fake server | Handles crashes, restarts, cancellation and server-to-client requests |
| 5.3 | `LspHub`, incremental document sync | After random edits, the server's view of the document matches ours |
| 5.4 | Diagnostics: underlines, gutter, Problems panel | Errors appear and clear as you type in all five languages |
| 5.5 | Go to definition and implementation, Cmd+click, chooser, back/forward | Works per the survey in every language |
| 5.6 | Find Usages panel, grouped by file with preview | Results update the editor on selection; Cmd+B on a definition shows usages |
| 5.7 | Completion, including auto-import and snippets | Accepting an item applies its additional edits as one undo step |
| 5.8 | Hover and signature help | Shows docs and the active parameter |
| 5.9 | File structure popup, LSP symbols in Search Everywhere | LSP results replace index entries without duplicates |
| 5.10 | Rename, code actions, format | Multi-file rename applies as one operation |

## M6: Debugger

Goal: IntelliJ-class debugging in all five languages.

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

## Later

Find in Files, Git integration, semantic highlighting, test runner, terminal panel, third-party
packages (stage 2), WASM plugins (stage 3), Windows.
