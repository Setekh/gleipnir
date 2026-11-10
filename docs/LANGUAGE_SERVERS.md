# Language servers: what they really do

Measured by `cargo run -p gleipnir-lsp --example survey -- <tools-folder>`, which starts each server
on a small sample project and tries every feature the editor offers. The sample has an interface or
trait with an implementation, usages, a function call, and one deliberate type error.

Versions: rust-analyzer 1.99.1, gopls v0.24.0, typescript-language-server 6.0.3 with TypeScript
6.0.3, ols dev-2026-06, csharp-ls 0.28.0 on .NET 10. Surveyed 2026-11-05, C# 2026-11-10.

| Feature | Rust | Go | TypeScript | Odin | C# |
|---|---|---|---|---|---|
| Go to definition | yes | yes | yes | yes | yes |
| Go to implementation | yes | yes | yes | **no** (not advertised; Odin has no interfaces) | yes |
| Go to type definition | yes | yes | yes | yes | yes |
| Find usages | yes | yes | yes | yes | yes |
| Hover | yes | yes | yes | yes | yes |
| Completion | yes | yes | yes | yes | yes |
| Signature help | yes | yes | yes | yes | yes |
| Document symbols | yes | yes | yes | yes | yes |
| Workspace symbols | yes | yes | yes | yes | yes |
| Rename | yes | **refuses while the package has errors** | yes | yes | yes |
| Format | yes (nothing to change in the sample) | yes (nothing to change) | yes | yes | yes (nothing to change) |
| Code actions | yes | yes | yes | **none returned** | yes |
| Diagnostics | yes, pushed | yes, pushed | yes, pushed | yes, pushed | yes, pushed |
| Position encoding | UTF-16 | UTF-16 | UTF-16 | UTF-16 | UTF-16 |

What this means for the editor:

- **Unsupported features are hidden or disabled per server**, by the capabilities each one
  advertises, rather than offered and failing. That covers Odin's missing go to implementation.
- **Errors from the server are shown as given.** gopls's refusal to rename explains itself
  ("not possible because ... has errors"), which is more useful than a generic failure.
- **Servers answer emptily while indexing.** The survey retries for up to 90 seconds for the first
  answer; rust-analyzer needed the longest. The editor shows a server as busy while it reports
  progress, so an empty answer then isn't mistaken for "nothing found".
- **UTF-16 everywhere**, which `gleipnir_text::Utf16Point` was built for.
