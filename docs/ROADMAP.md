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
| 2.5 | Project panel: file ops, reveal active file (Alt+F1, or the target in its header), gitignored and git status colours | Create, rename, delete and move all work, with confirmation for delete |
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
- **Find and Replace in a file** (Cmd+F, Cmd+R; Ctrl on Linux): a bar over the editor finds as
  you type from the cursor, marks every match, and shows which of how many it's on. Enter and
  Shift+Enter, Cmd+G and Cmd+Shift+G, or F3 and Shift+F3 step through them, around the ends; Alt+C,
  Alt+W and Alt+X toggle Match Case, Words and Regex. Replace does the match it's on and moves on
  (Enter in the replacement); Replace All (Alt+A) is one undo step, and a pattern's `$1` fills in
  what it caught. Select All (Ctrl+Cmd+G) puts a cursor on every match. Escape leaves the match
  selected, and F3 searches again with the bar closed; the last search carries over to other files.
  A selection over several lines opens it In Selection, which follows edits and toggles off;
  Preserve Case keeps each match's capitals; Up and Down in either field bring back earlier
  searches and replacements.
- **Replace in Files** (Cmd+Shift+R, Ctrl+Shift+R on Linux): Find in Files with what to replace
  with (Tab moves between the fields) and Preserve Case. Replace does the selected match; Replace
  All (Alt+A) asks once with how many in how many files, then replaces. Open files change as one
  undo step and stay unsaved; others change and are saved, as a rename does. It covers the files
  found, so past 5,000 matches some may be left.

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

Done after that, closing what M10 left open:

| # | Item | Finished when |
| --- | --- | --- |
| 10.7 | The log's graph: a coloured lane per branch, dots for commits, lines to their parents, merges joining in; only the rows on screen are drawn | A branch merged back shows in its own lane, meeting the first one at the fork and the merge |
| 10.8 | Annotate with Git Blame, from the gutter's right-click menu, the Git menu or Find Action: the day and author beside each line, tinted newer to older; resting on one shows the commit, clicking chooses it in the log | Labels follow edits at once, and are blamed again against the unsaved text when typing stops |
| 10.9 | Interactively Rebase from Here, from a commit's menu in the log: pick, reword, squash, fixup or drop each commit, reorder them, edit messages; a conflict stops it in the Commit window, with Continue and Abort | Real rebases on temporary repositories reword, squash, drop and reorder, and continue through conflicts |

The diff window has IntelliJ's toolbar: previous and next difference, Jump to Source (F4 or
Cmd+Down) opening the file at the change, a side-by-side or unified viewer, whitespace compared,
trimmed, ignored, or ignored with blank lines, changed words highlighted within changed lines (or
just the lines, or nothing), and unchanged runs folded to four lines either side of each change,
opened by a click. The choices are kept in `settings.json` under `diff`. The code scrolls sideways
with a sideways swipe, or on macOS Shift and the wheel. With Sync scrolling off, each side scrolls
on its own, down and sideways; F7 still brings both to the change.

How each works is under Git in [ARCHITECTURE.md](ARCHITECTURE.md). `script/e2e git` now also checks
the graph and turns blame on and off. In the rebase window, P, R, S, F and D set a commit's action,
Alt+Up and Alt+Down move it, and Cmd+Enter starts. Labels in the gutter right-click for Show in Git
Log, Copy Hash and Close Annotations.

Fixed alongside: the Commit window's own `git status` could hold the index lock just as a commit
started, which failed it now and then (`script/e2e git` showed it about one run in three); git is
now always run with `GIT_OPTIONAL_LOCKS=0`, as the project's status already was.

Still open from M10:

- **Merges in a rebase**: rebasing from before a merge is refused, rather than flattening it;
  `--rebase-merges` and the `edit` action aren't offered.
- **Blame of older revisions** (annotating the previous revision from a label, as IntelliJ can) isn't
  there, and a commit older than the log's 2,000 can't be chosen in it from a label.

## M11: Inline suggestions

Goal: Copilot's grey suggestions as you type, with how likely each is, and nothing leaving the
machine: cheap ones from the file itself, and a small code model run locally by llama.cpp.

Status: built and passing on macOS. `script/e2e suggest` has a field among comma-ended fields
offered its comma at 100% and taken with Tab, from the file alone. With the model, on a copy of the
store that prompted this, `address2: types.maybe(Provisioning` was offered `Address),` at 98%,
from Qwen2.5-Coder 0.5B under llama.cpp b11889, both installed by Gleipnir and checksummed;
`GLEIPNIR_LLAMA_TOOLS=<tools folder> cargo test -p gleipnir-suggest` runs that against the real
model. Warm requests take about 15 ms. Typing is no slower with suggestions on than off.

Several lines: after `for (const item of items) {`, the real model offers the body and the
closing brace at 67%, in rows of their own; Cmd+Right takes a line of it and Option+Right a word.
Shown `formatAddress`'s definition from another open file, it writes `formatAddress(order.address,
', ')` with the separator argument it otherwise leaves out, in about 45 ms.

Still open from M11:

- **Long suggestions score low.** A score is the probability of the whole text, so several lines
  rarely clear the default 50% and are cut back to those that do; a lower minimum shows more.
- **Suggestions wait while a completion list is open**, as Tab picks from the list then.
- **llama.cpp on Linux is untried**; only the release's layout was checked.

| # | Item | Finished when |
| --- | --- | --- |
| 11.1 | Grey text after the cursor with its score; Tab takes it, Esc drops it, typing what it says keeps it | A suggestion shows, is taken with Tab as one undo step, and goes when the cursor moves |
| 11.2 | Suggestions without a model: how the same text goes on elsewhere in the file, and the punctuation sibling lines end with, scored by how often they agree | After `address2: types.maybe(ProvisioningAddress)` among lines ending in `,`, a `,` is offered |
| 11.3 | A local model: llama.cpp's server and a small fill-in-the-middle code model, installed when first needed and checksummed, scored by the model's own odds | A line is suggested from the code before and after the cursor, with its probability |
| 11.4 | Settings: on or off, which model, and how sure a suggestion must be to show | Each setting takes effect at once |
| 11.5 | Several lines, on a blank line or after a block's opener, in rows made for them; taken a word or a line at a time | A loop's body and closer are offered after its `{`, and Cmd+Right / Option+Right take a line / a word |
| 11.6 | The model shown related pieces of the other open files | A function from another open file is called with its real parameters |

## M12: A terminal, and coding agents in it

Goal: a terminal panel like IntelliJ's, and coding agents (Claude Code first, then Codex) running
in it as they would anywhere, but able to see and work in the IDE: what's selected, the problems,
the tests, the open files, and edits made on the spot or offered for review.

Each agent is the person's own installed CLI, signed in by them in its own way: Gleipnir never
handles credentials, never stands between an agent and its service, and installs no agent by
itself. It only adds tools, through MCP, which every such agent supports, and instructions telling
an agent how to use them. The protocol Claude Code's `/ide` speaks with the VS Code and JetBrains
plugins isn't used: it's undocumented for other editors and could change.

Status: in progress. The terminal is built: `script/e2e terminal` opens a shell, types into it,
stops a program with Ctrl+C, runs vim full-screen and quits it, and opens and closes tabs. Agent
tabs start Claude Code and Codex through the login shell; Claude Code was seen starting in one,
signed in with the person's own account. Programs that ask for the mouse, like Claude Code's
full-screen view, vim and htop, are sent its clicks, drags and wheel as xterm sends them; Shift
keeps it for selecting. Send to Agent types `@file#L3-7` into the agent's tab. Agents have their
own tool window on the right, which offers them when empty and never starts one unasked; as agents
don't start again with the window, it doesn't reopen with it either.

Gleipnir's MCP server and its fifteen tools (12.3 to 12.5) are built. Each
window listens on its own socket, and `script/e2e agent` calls the tools as an agent does, by
starting `gleipnir mcp`: it lists the open files, reads the line the cursor is on, edits the open
file, finds the edit there unsaved and undoes it, is refused a file outside the project, and shows
a selection. The headless tests drive every tool against a real workspace, a fake language server
and the real test runner, and two clients at once through the socket.

Agent tabs give their agent the window's server: Claude Code with `--mcp-config` and the skill
with `--plugin-dir`, Codex with `-c mcp_servers.gleipnir…` overrides, so nothing is written to
either's configuration. In a real Claude Code tab, `/mcp` lists `gleipnir` connected with its 15
tools, and the skill is offered as `/gleipnir:gleipnir`; neither check reached the model.

Still open from M12:

- **No agent has done real work through the tools yet.** Checking that would spend the person's
  usage, so it's for them to try: ask Claude Code in its tab about "this", or to fix a problem.
- **Codex's connection is untested live**: the check was interrupted by Codex's own update prompt.
- **Typing in the terminal goes by keys**, without an input method, so composed characters (dead
  keys, Chinese and the like) can't be typed there yet; Option's own characters can.

- **A proposed edit waits 50 seconds for the person**, then answers "pending" with an id that
  `get_proposal` waits on again, as Codex gives up on a tool call after a minute by default.
- **A program run with `run_configuration`** is answered with its output so far once it ends or
  the wait runs out; what it prints after that is only in the Run panel.
- **Problems** are those the language servers have reported, which is mostly for open files.

| # | Item | Finished when |
| --- | --- | --- |
| 12.1 | Terminal panel (Alt+F12): tabs, each a shell in the project folder, with colours, scrollback, selection, copy and paste, links to files, resizing; on alacritty_terminal (Apache-2.0) | A shell runs commands, full-screen programs like `vim` and `htop` draw properly, and a `path:line` printed by a build opens the file there |
| 12.2 | Agent tabs: New Claude Code (and later Codex) tab, running the installed CLI in the project folder; a missing one says how to install it | A Claude Code tab starts `claude`, which asks to log in if it hasn't been |
| 12.3 | Gleipnir's MCP server: each window listens on a socket only its user can open, and `gleipnir mcp` connects an agent to it; agent tabs pass it to their agent without writing to the project | An agent tab's agent lists Gleipnir's tools |
| 12.4 | What the IDE knows, as tools: open files, cursor and selection, problems, test results, Git status, definitions and usages | Asked about "this", an agent reads the selection; asked to fix the problems, it reads them from the IDE |
| 12.5 | Acting in the IDE, as tools: open a file at a line, edit an open file's text (unsaved edits kept, one undo step), run a test or configuration, and `propose_edit`, a diff reviewed change by change | An edit lands in the open buffer and Undo takes it back; a proposed edit is accepted in part |
| 12.6 | Send to Agent (Cmd+Alt+K): the selection, as a file and lines reference, typed into the active agent tab | The reference arrives at the prompt, and the agent reads those lines |
| 12.7 | Instructions for agents in Gleipnir, one text for all: the MCP server's own instructions; for Claude Code a skill in a plugin passed with `--plugin-dir`, and a line saying where it runs; for Codex the skill's text as the session's developer instructions; so nothing is installed into the person's config | A new agent session uses the IDE's tools without being told to |
| 12.8 | Codex: its tab, and the MCP server and instructions given to it its own way | A Codex tab's agent edits an open file through Gleipnir |

## Later

WASM plugins (stage 3), Windows.
