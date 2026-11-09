# Gleipnir

An IDE written in Rust on [GPUI](https://gpui.rs), for TypeScript, JavaScript, Odin, Rust and Go.

Status: a project-based editor with a file tree, tabs, split panes, multi-cursor editing,
IntelliJ-style Search Everywhere, per-project SDKs with managed tool installs, and code intelligence
from language servers: completion, documentation, go to declaration and implementations, usages,
problems, rename, quick fixes and reformatting. Programs run and debug from the IDE in all five
languages, IntelliJ style: breakpoints in the gutter (conditions, hit counts, logpoints), stepping,
frames, a variables tree with watches, values shown beside the code, and a console. See [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md)
for the design and [docs/ROADMAP.md](docs/ROADMAP.md) for the plan.

## Building

Requires [rustup](https://rustup.rs); `rust-toolchain.toml` selects the compiler version. On macOS you
also need Xcode. On Arch or Omarchy, `sudo script/linux-deps` installs the system packages.

    cargo run -p gleipnir -- path/to/project   # a folder, or a file in one; the first build takes minutes
    cargo test --workspace

## Testing

    script/check                # formatting, lints and all tests: run before committing
    cargo test --workspace      # unit, property and headless UI tests
    script/e2e                  # drives the real app; needs a display
    PROFILE=release script/e2e perf
    script/e2e code             # real language servers, installed into target/e2e-tools as needed
    script/e2e debug            # real debuggers, likewise; E2E_TOOLS=<folder> uses another folder
    script/e2e tests            # the test runner, with cargo, go, odin and npm on the PATH
    GLEIPNIR_TOOLS=~/path/to/state/tools cargo test -p gleipnir-dap --test adapters   # headless
    script/linux-check          # builds and tests on Arch in Docker, from any machine

Property tests run 256 cases by default. `PROPTEST_CASES=20000 cargo test -p gleipnir-text --release`
searches much harder, and has found real bugs that the default run missed.

## Layout

- `crates/gleipnir`: the application, and the end-to-end test driver.
- `crates/workspace`: the window: panes, tabs, docks, status bar, saved layouts, the keymap file.
- `crates/project_panel`: the project tree.
- `crates/search`: Search Everywhere, its tabs, and the double-Shift gesture.
- `crates/settings_ui`: Languages & SDKs, for choosing SDKs and installing tools.
- `crates/code_nav`: code intelligence from language servers: the editor's completion and
  documentation, navigation, the Find and Problems panels, rename, intentions and reformatting.
- `crates/debugger_ui`: running and debugging: the Run and Debug panels, the run widget in the
  status bar, breakpoints and values in the editor, the breakpoint and Evaluate popups, and test
  results.
- `crates/dap`: the debug adapter client: stdio and TCP adapters, child connections for js-debug,
  the launch handshake and session state.
- `crates/lsp`: the language server client: framing, requests with cancellation, and the
  capabilities we advertise.
- `crates/packages`: language manifests (in `builtin/`), SDK detection, tool resolution and
  installation, and each language's run and debug adapter.
- `crates/editor`: the editor view: rendering, input, mouse and key bindings.
- `crates/ui`: theme, icons, text input, context menu and list navigation.
- `crates/core`: open buffers, file loading and saving, external change detection, the project
  (its files, symbol index and git status, kept current by watching it), and the language servers:
  starting and restarting them, document sync, diagnostics and requests, and the debugger: run
  targets, builds, launches, debug sessions and the project's breakpoints.
- `crates/index`: the file tree model, file list, fuzzy matching and search, gitignore rules, git
  status, file watching and file operations.
- `crates/syntax`: languages, background parsing, highlighting, structural selection and symbol
  extraction.
- `crates/text`: rope buffer, undo, selections, and every editing command as a pure function.
- `crates/test_support`: helpers shared by the UI tests.

## Configuration

Settings (`Cmd+,`, or the gear in the toolbar) has pages for appearance, the editor, code style,
the keymap, languages and tools, run and debug, and packages, with a search over all of them.
Changes take effect at once and are kept in `~/.config/gleipnir/settings.json`.
Gleipnir also reads `settings.json` again whenever it changes, so it can be edited by hand.

Themes are TOML files in `~/.config/gleipnir/themes`, which has a README with every key and full
examples of the built-in themes. A theme saved there, or changed, shows at once, so an AI agent can
make or tweak one by writing the file and switch to it in `settings.json`:

    name = "Midnight"
    appearance = "dark"     # the built-in theme anything left out comes from

    [colors]
    background = "#0b0e14"
    accent = "#7aa2f7"

    [syntax]
    keyword = "#bb9af7"

Indentation follows the project where it says: `.editorconfig`, and for the files Prettier formats,
Prettier's config (`.prettierrc` in JSON, JSON5, YAML or TOML, or `package.json`'s `prettier`),
over the language's own and the Code Style settings.

Key bindings added in Settings, or by hand, are kept in `~/.config/gleipnir/keymap.conf`, one per
line:

    # keys = action, optionally `in` a context
    cmd-k = editor::DuplicateLine
    ctrl-j = editor::MoveDown in Editor

Mistakes are reported in the status bar, and don't stop the rest of the file from loading.

Languages & Tools (`Cmd+;`) shows each language's SDK and tools. Tools Gleipnir installs go into its
state folder, never system-wide, the first time they're needed, unless that's turned off in
Settings. Each project's layout is saved outside
the project, in `~/Library/Application Support/Gleipnir` on macOS
and `~/.local/state/gleipnir` on Linux.

## Build notes

- GPUI is pinned to one Zed commit in the root `Cargo.toml`. The crates.io release is far behind
  and GPUI's API changes often, so upgrade it deliberately.
- GPUI's `runtime_shaders` feature is on, so shaders compile at launch and the build doesn't need
  Apple's Metal Toolchain. For precompiled shaders, run
  `xcodebuild -downloadComponent MetalToolchain` and remove that feature.
