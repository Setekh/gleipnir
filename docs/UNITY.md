# Unity

A folder with `ProjectSettings/ProjectVersion.txt` beside `Assets` is a Unity project. A file in one
opens with the Unity project as its project, even when that's a folder inside a larger repository,
because Unity writes the project's solution there.

## What Gleipnir does in one

- **C#** with csharp-ls, on the solution Unity writes: completion, documentation, navigation,
  usages, rename, quick fixes and problems, as for the other languages.
- **`.meta` files go with their assets.** Unity knows each asset and folder by the GUID in its
  `.meta` file, which scenes and prefabs refer to. Renaming, moving or deleting from the project
  tree does the same to the `.meta`, as Unity's own editor does, so nothing loses its references.
  Deleting moves both to the Trash, so putting both back restores the asset as it was.
- **What Unity makes is out of the way**: `.meta` files aren't shown, and `Library`, `Temp`,
  `Logs`, `obj`, `UserSettings` and `MemoryCaptures` count as ignored, dimmed in the tree and left
  out of search and file watching, even if the project's `.gitignore` doesn't list them.
- **Unity's other text files**: UI Toolkit's style sheets (`.uss`) as CSS, its layouts (`.uxml`)
  highlighted as HTML, and assembly definitions (`.asmdef`, `.asmref`) and input actions
  (`.inputactions`) as JSON.

## Setting up

You need the .NET SDK (8 or later), which csharp-ls runs on. Gleipnir finds it on `PATH` or where
its installers put it, and installs csharp-ls itself, from Languages & SDKs, if it isn't already.

**Gleipnir as Unity's script editor.** In Unity, open Preferences (Settings on macOS), then
External Tools. Under External Script Editor choose Browse… and pick the `gleipnir` binary, and
set External Script Editor Args to:

    "$(File)" --line $(Line) --column $(Column)

Opening a script from Unity then shows it at that line in the Gleipnir window that has the project
open, and brings it to the front. With none open, a new one starts. The same works from a terminal:
`gleipnir Assets/Scripts/Player.cs:42`.

**The solution.** csharp-ls reads the `.sln` and `.csproj` files at the project's root, which
Unity writes through the package for the chosen external editor. Gleipnir has no such package, so
with it chosen Unity may stop writing them. Write them once with a supported editor chosen, using
the Regenerate project files button in External Tools, and again if csharp-ls doesn't see scripts
or packages added since.

## Not yet

- Debugging a running Unity player or the editor. There's no open debug adapter for Unity's
  runtime to build on.
- Running C# programs, and C# tests.
- Unity's scene, prefab and asset files, which are YAML, aren't highlighted.
