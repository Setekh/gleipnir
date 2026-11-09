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


## M5: Code intelligence

Goal: daily-driver code understanding. **After this, Gleipnir can be used to develop itself.**

Status: built and passing on macOS, against fakes in tests and the real servers end to end:
`script/e2e code` drives rust-analyzer, gopls,
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
`script/e2e debug` debugs Rust and Odin through lldb-dap, Go
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

Done in the third batch:

- **Rust values read as Rust** under lldb: launches load the toolchain's own formatters, as
  `rust-lldb` does, so a `String` shows its text rather than `alloc::string::String @ 0x...`.
- **Programs can be typed to.** A run's input line writes to its stdin. Under the debugger, js-debug
  and Run start the program through Gleipnir (`runInTerminal`), so their input goes straight to it;
  lldb keeps the program on a terminal of its own, which lldb types into for us. In the Debug
  console, a line is evaluated while stopped and typed to the program while it runs.
- **Values can be changed** while stopped: double-click a variable.
- **Exception breakpoints**: each debugger's kinds of exception, once seen, are listed under View
  Breakpoints to turn on or off, now and for later sessions.
- **Run configurations can be edited** (Run > Edit Configuration): arguments, environment and
  working folder, saved with the project.
- **Tests are run targets**: each Rust package's and Go package's tests, and, from a Run icon in
  the gutter, one `#[test]` or `func TestX`, run or debugged. Debugging a Rust test asks Cargo for
  the test binary's name; Go tests debug in delve's test mode.
- **Attach to Process** (Alt+Shift+F5): pick a process and a debugger. js-debug attaches to Node by
  having Gleipnir open its inspector; lldb attaches by pid, which macOS allows only with Developer
  Mode or `DevToolsSecurity` enabled.
- **Third-party packages** (stage 2): a folder with a `package.toml` in the config folder's
  `packages`, installed with Install Package, adds a package or replaces a built-in one, such as to
  use a different language server. Its languages must use one of the built-in grammars, as loading
  new ones needs WASM (stage 3).

Still open after M7: typing to a Go program under the debugger, as delve gives no way in; and the
items above that need an Omarchy machine or a remote. Odin tests as targets were done in M8.

## M8: Test runner

Goal: IntelliJ's test runner for Rust, Go and Odin: a live tree of results, each test's own output,
a jump to its source, and rerunning what failed.

Status: built and passing on macOS. `script/e2e tests` runs real cargo, go and odin: a Rust test
module from the cursor, a failing test's own output, red and green gutter icons, a fix and Rerun
Failed, a crate's unit and integration tests together, a Go test with subtests, and Odin's tests by
package and one by one. The headless tests in `crates/dap/tests` debug one test in each of the three.
How it works is under Test runner in [ARCHITECTURE.md](ARCHITECTURE.md).

| # | Item | Finished when |
| --- | --- | --- |
| 8.1 | Results from test output: libtest's text, `go test -json`, Go's `-test.v` under delve, Odin's runner log | Each builds the right tree from recorded real output |
| 8.2 | Test results view in the Run and Debug panels: tree with states, counts and times, the selected test's output beside it | A run fills the tree live; selecting a test shows only its output |
| 8.3 | Navigation: a test jumps to its source; `file:line` in any console opens the file | Double-click or Enter on a test opens it; a panic's location is a link |
| 8.4 | Rerun failed tests, and run or debug any test, module or package from the tree | Rerun Failed runs exactly the failed tests |
| 8.5 | Gutter icons show each test's last result; Ctrl+Shift+R/D in a test runs or debugs it; `mod tests` runs its module; an All Tests configuration | The icon turns red after a failing run, green after a fix |
| 8.6 | Odin tests as run targets | A package's `@(test)` procedures run and debug like Rust's and Go's |
| 8.7 | End-to-end `tests` script against real cargo, go and odin | Passes |

Shortcuts, IntelliJ's: Ctrl+Shift+R and Ctrl+Shift+D run or debug the test the cursor is in, or its
module, falling back to the file; in the results, Enter or F4 jumps to the source and the arrows
move and fold. Rerun Failed Tests is in the Run menu and on the results' toolbar, beside the toggle
for passed tests. A right-click on a result runs or debugs it.

Fixed alongside M8:

- **Missing tools install themselves** when first needed: a language server when a file in its
  language opens, a debugger when debugging starts. The status bar shows "installing …"; a failed
  install says why and isn't retried until asked. `script/e2e code` and `debug` now set up their own
  tools, kept in `target/e2e-tools`.
- **A tool window's rail button hides it when it's open**, focused or not, as IntelliJ's do. Before,
  it only hid a panel that had focus, so one opened by Find Usages couldn't be closed by clicking.

Still open from M8:

- **Durations of single Rust tests**: stable libtest prints them only per binary.
- JavaScript and TypeScript test results, done in M9 for jest and vitest. Node's own runner isn't
  read yet.

## M9: Working like IntelliJ day to day

Goal: the rough edges found using Gleipnir on a real TypeScript monorepo, smoothed out.

Status: built and passing on macOS. `script/e2e tests` runs a real jest project: a file's tests,
the tree with its describes, a failure's own output and the gutter's marks, one test from the
cursor, Rerun Failed, and the test under the debugger stopping at a breakpoint in it. On the Nx
monorepo that prompted this, with jest 29 and babel, one `it` and one `describe` run alone, results
arrive test by test, and a TypeScript test stops at its breakpoint. Completion there shows 0.27 s
after the first key, against 7.9 s before the warm-up, with typing at 6.5 ms a key in a release
build while the list follows it (`type-timed` in the driver measures this). Settings' pages each
have tests that a change takes effect and is kept.

| # | Item | Finished when |
| --- | --- | --- |
| 9.1 | JavaScript and TypeScript tests: a run icon on every `describe`, `it` and `test`, to run or debug just that one, with jest or vitest; Ctrl+Shift+R/D at the cursor; results in the tree | A failing `it` runs alone from its icon, shows red, and debugs to a breakpoint inside it |
| 9.2 | Choose Declaration, Implementation and Usages open beside the click or caret, not at the top, and close the hover | The chooser's top-left is just below the caret's line |
| 9.3 | Completion pops up while typing, without slowing typing: one request per word, and slow servers warmed up when the first file opens | Typing an identifier shows suggestions in under a second, even first thing; keystroke time stays as before in `perf` |
| 9.4 | IntelliJ's main toolbar in place of the OS title bar: project with recent projects, the Git branch, the run configuration with Run, Debug and Stop, Search Everywhere and Settings | Clicking each opens what it should; the window still drags and zooms from the bar |
| 9.5 | A Settings screen for everything: editor, appearance, keymap, languages and tools, run and debug | Each setting changes the app live and is kept |

Done alongside M9: the branch in the toolbar switches branch; jest results arrive test by test,
through a reporter Gleipnir adds beside jest's own (vitest's still come at the end); and `it.each`
and `describe.each` get Run icons that run every case.

Also done alongside M9, after it: themes as files that are read again as they change, with a
README for people and AI agents in the themes folder (`script/e2e themes` makes, changes and breaks
one); `settings.json` read again as it changes; the branch menu creating, checking out, merging,
rebasing and deleting branches; indentation from `.editorconfig` and Prettier's config. The one-off
slow keystroke seen on the first completion list didn't come back over two runs of 172 keys on the
same project; `type-timed` now names any key slower than a frame.

Still open in M9:

- Prettier configs in JavaScript are read now, by running them with Node as Prettier does, ahead
  of time when a project opens; TypeScript ones (`prettier.config.ts`) still aren't.

## M10: Git

Goal: IntelliJ's everyday Git: see what changed as you edit, commit it, push and pull, look back,
and settle conflicts, without a terminal.

Status: built and passing on macOS. Each git operation is tested against a real repository (a push
to a local bare remote, a real merge conflict), the Git windows through the UI, and `script/e2e
git` drives the real app: a change marked in the gutter as it's typed, its diff, a commit from the
Commit window, the marks clearing, and the commit in the log.

Shortcuts, IntelliJ's: Cmd+K commit, Cmd+Shift+K push, Cmd+T update project, Cmd+0 the Commit
window, Cmd+9 the Git log, Cmd+Alt+Z rollback lines, Cmd+Enter in the message to commit, and F7 and
Shift+F7 through a diff's changes. The branch menu starts with Update Project, Commit and Push, as
IntelliJ's does, and there's a Git menu.

| # | Item | Finished when |
| --- | --- | --- |
| 10.1 | Change marks in the gutter against the last commit, kept current while typing | Added, changed and deleted lines show their marks as edits are made, and clear on commit |
| 10.2 | Commit panel (Cmd+K): changed files to tick, a message, Commit, Commit and Push, Amend | A commit of the ticked files leaves the others changed |
| 10.3 | A file's changes as a diff, from the commit panel and the gutter, with Rollback | Rolling back a change restores the committed lines |
| 10.4 | Push (Cmd+Shift+K) and Update Project (Cmd+T), with git's messages | Both run in the background and say how it went |
| 10.5 | History: the log, and a commit's changed files and their diffs | Selecting a commit shows its files; a file shows its diff |
| 10.6 | Conflicts: listed in the commit panel, accept yours or theirs, or open the file to resolve | Accepting a side resolves the file and stages it |

Done after M10's first part: diffs highlighted as code, with ⟲ to put one change back at a time;
a three-way merge window for conflicts (yours, the result, theirs; one-sided changes merged, each
conflict settled by a side or both, Apply to save and stage), on a line merge that's property-tested;
the log of every branch with branch and tag labels, and a commit's menu with Cherry-Pick, Revert,
New Branch Here, New Tag Here and Copy Hash; Stash and Unstash; push taking annotated tags along;
and Prettier configs in TypeScript.

Still open from M10:

- **The log has no graph** of how branches join, and no blame (annotate) in the editor.
- **No interactive rebase**, squash or reword.

## M11: Inline suggestions

Goal: Copilot's grey suggestions as you type, with how likely each is, and nothing leaving the
machine: cheap ones from the file itself, and a small code model run locally by llama.cpp.

Status: built and passing on macOS. `script/e2e suggest` has a field among comma-ended fields
offered its comma at 100% and taken with Tab, from the file alone. With the model, on a copy of the
store that prompted this, `address2: types.maybe(Provisioning` was offered `Address),` at 98%,
from Qwen2.5-Coder 0.5B under llama.cpp b11889, both installed by Gleipnir and checksummed;
`GLEIPNIR_LLAMA_TOOLS=<tools folder> cargo test -p gleipnir-suggest` runs that against the real
model. Warm requests take about 15 ms. Typing is no slower with suggestions on than off.

Still open from M11:

- **One line at a time.** Multi-line suggestions, and taking a suggestion a word at a time, aren't
  there yet.
- **Only the file at the cursor is context** for the model; other open files aren't sent to it.

| # | Item | Finished when |
| --- | --- | --- |
| 11.1 | Grey text after the cursor with its score; Tab takes it, Esc drops it, typing what it says keeps it | A suggestion shows, is taken with Tab as one undo step, and goes when the cursor moves |
| 11.2 | Suggestions without a model: how the same text goes on elsewhere in the file, and the punctuation sibling lines end with, scored by how often they agree | After `address2: types.maybe(ProvisioningAddress)` among lines ending in `,`, a `,` is offered |
| 11.3 | A local model: llama.cpp's server and a small fill-in-the-middle code model, installed when first needed and checksummed, scored by the model's own odds | A line is suggested from the code before and after the cursor, with its probability |
| 11.4 | Settings: on or off, which model, and how sure a suggestion must be to show | Each setting takes effect at once |

## Later

Git integration, terminal panel, WASM plugins (stage 3), Windows.
