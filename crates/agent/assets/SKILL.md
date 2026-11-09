---
name: gleipnir
description: Working inside the Gleipnir IDE, where the gleipnir MCP tools see and change what the person has open. Use whenever those tools are available - for anything about "this", "here" or "the selection", for edits to files open in the IDE, for changes the person should review first, and for tests, problems and Git state.
---

# Working in Gleipnir

This session runs in a terminal tab of Gleipnir, an IntelliJ-like IDE, with the person's project
open. The `gleipnir` MCP server's tools reach into it. Prefer them to guessing or to the shell when
they answer the question, as they see what the person sees, including edits they haven't saved.

## What the person means

- "This", "here", "the selection", "this function": call `get_selection` first. It has the file,
  the cursor and the selected text with its lines. `get_open_files` lists their tabs and which
  have unsaved edits.
- A reference like `@src/store.ts#L40-52` typed into the prompt came from Gleipnir's Send to Agent
  (Cmd+Alt+K): those are the lines they had selected.

## Reading

- `read_file` gives a file as the editor has it, unsaved edits included. For a file that's open
  and dirty, the copy on disk is out of date; read it this way.
- `get_problems` has the language servers' errors and warnings, positioned for the unsaved text.
  Check it after editing, before saying something compiles.
- `find_definition` and `find_usages` ask the language server; prefer them to text search for
  code.

## Changing code

- For a file open in Gleipnir, change it with `edit_file`, not by writing the file: the change
  lands in the open editor as one step the person can undo, keeps their own unsaved edits, and
  stays unsaved until `save_file` or they save. Writing an open, dirty file on disk would clash
  with their edits.
- When they ask for a suggestion, a proposal, or to see a change before it's made, or the change
  is large or risky, use `propose_edit`. They review it as a diff and accept or reject each change.
  It waits for them for a while; if it answers `pending`, carry on with other work or wait, and
  call `get_proposal` with its id for the decision. Only the accepted changes are applied: read the
  answer before relying on them.
- Files that aren't open can be changed either way; `edit_file` opens them in a background tab.
- `open_file` shows the person a place, as when pointing them at something.

## Checking

- `run_tests` runs a file's tests, or one by name or line, with the IDE's test runner, so the
  results show in its gutter and Run panel too; `get_test_results` has the last run's failures and
  their output.
- `run_configuration` lists the project's run configurations and runs one.
- `get_git_status` has the branch, the changed files and any merge or rebase under way.

Paths are relative to the project root, and only files inside it can be used. Lines and columns
count from 1.
