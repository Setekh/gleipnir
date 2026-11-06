# Debuggers: what each adapter needed

Found by running each adapter headless (`GLEIPNIR_TOOLS=<tools folder> cargo test -p gleipnir-dap
--test adapters`) and in the app (`script/e2e debug`). Each test builds a small program, stops at a
breakpoint, checks the frame's file and line, reads and evaluates a local, steps over, and runs to
the end. Versions: lldb-dap from Xcode 26, delve 1.27, js-debug 1.140 on Node 24, Odin dev-2026-06.
Measured 2026-11-06.

| | Rust | Odin | Go | JavaScript | TypeScript |
| --- | --- | --- | --- | --- | --- |
| Adapter | lldb-dap (stdio) | lldb-dap (stdio) | `dlv dap` (TCP) | js-debug (TCP) | js-debug (TCP) |
| Built first | `cargo build --bin` | `odin build -debug` | delve builds it | no | no, or `tsc` |
| Stop, frame, locals, step | yes | yes | yes | yes, in a child session | yes, run by Node directly or through source maps |
| Conditions | sent, not yet exercised | sent, not yet exercised | sent, not yet exercised | sent, not yet exercised | yes, end to end |

Hit counts and logpoints are sent the same way as conditions, but no test has exercised them
against a real adapter yet.

What it took:

- **The handshake order differs.** lldb-dap and js-debug answer `launch` only after
  `configurationDone`; delve answers `launch` first and then sends `initialized`. The client sends
  the launch without waiting for its answer, configures on `initialized`, and only then awaits it.
- **js-debug debugs in a child session.** The first connection's launch is answered with a
  `startDebugging` request; the client connects to the same port again and launches the
  configuration it was given. Breakpoints go to both, but only the child stops. js-debug doesn't
  answer `disconnect` once its sessions have ended, so stopping has a time limit.
- **js-debug needs `__workspaceFolder`.** Its standalone server builds its defaults from VS Code's
  workspace folder, including where compiled files are. Without it, breakpoints in TypeScript
  never resolve through source maps. The launch sets it to the project root, along with `outFiles`.
- **delve keeps the program's output to itself** unless the launch says `outputMode: "remote"`.
- **lldb-dap on macOS needs permission to debug.** With Developer Mode off, the first launch asks for
  an administrator's password, and the launch times out with "Resume timed out" if nobody answers.
  `sudo DevToolsSecurity -enable` avoids the prompt.
- **lldb has no Rust plugin**, so it says so in the console and shows Rust values in C terms: an
  `f64` is a `double`. Locals and stepping work.
- **Odin writes its binary where told**, but not into a folder that doesn't exist, so the folder is
  made before building.
- **Paths come back as the program saw them.** On macOS that can be `/var/...` for `/private/var/...`,
  so frames' paths are made canonical to match the editor's.
