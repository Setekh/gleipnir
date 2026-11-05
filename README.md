# Gleipnir

An IDE written in Rust on [GPUI](https://gpui.rs), for TypeScript, JavaScript, Odin, Rust and Go.

Status: a project-based editor with a file tree, tabs, split panes and multi-cursor editing.
Search, code intelligence and debugging are next. See [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md)
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
    script/linux-check          # builds and tests on Arch in Docker, from any machine

Property tests run 256 cases by default. `PROPTEST_CASES=20000 cargo test -p gleipnir-text --release`
searches much harder, and has found real bugs that the default run missed.

## Layout

- `crates/gleipnir`: the application, and the end-to-end test driver.
- `crates/workspace`: the window: panes, tabs, docks, status bar, saved layouts, the keymap file.
- `crates/project_panel`: the project tree.
- `crates/editor`: the editor view: rendering, input, mouse and key bindings.
- `crates/ui`: theme, icons, text input, context menu and list navigation.
- `crates/core`: open buffers, file loading and saving, external change detection.
- `crates/index`: the file tree model, gitignore rules, git status, file watching and file operations.
- `crates/syntax`: languages, background parsing, highlighting and structural selection.
- `crates/text`: rope buffer, undo, selections, and every editing command as a pure function.
- `crates/test_support`: helpers shared by the UI tests.

## Configuration

Key bindings can be changed in `~/.config/gleipnir/keymap.conf`, one per line:

    # keys = action, optionally `in` a context
    cmd-k = editor::DuplicateLine
    ctrl-j = editor::MoveDown in Editor

Mistakes are reported in the status bar, and don't stop the rest of the file from loading. Each
project's layout is saved outside the project, in `~/Library/Application Support/Gleipnir` on macOS
and `~/.local/state/gleipnir` on Linux.

## Build notes

- GPUI is pinned to one Zed commit in the root `Cargo.toml`. The crates.io release is far behind
  and GPUI's API changes often, so upgrade it deliberately.
- GPUI's `runtime_shaders` feature is on, so shaders compile at launch and the build doesn't need
  Apple's Metal Toolchain. For precompiled shaders, run
  `xcodebuild -downloadComponent MetalToolchain` and remove that feature.
