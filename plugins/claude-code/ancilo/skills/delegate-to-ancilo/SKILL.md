---
name: delegate-to-ancilo
description: Hand well-defined, mechanical coding work to Ancilo's local model (free, private, runs in parallel) – writing or extending tests, boilerplate, pattern-based refactorings, renames, docs and comments, small well-specified fixes, summarising files or logs. Use it whenever such a sub-task can be described precisely; keep architecture, subtle debugging and judgement calls yourself.
---

# Delegating to Ancilo

Ancilo runs a language model on this machine. Delegating saves your context and the user's API budget, keeps code local, and lets work happen in parallel.

## When to delegate

Delegate when the sub-task is **well-defined and verifiable**:

- write or extend tests for a given function or module
- boilerplate: new endpoints, handlers, types following an existing example
- mechanical refactorings with a clear pattern (rename, extract, move, convert)
- documentation, comments, README sections
- small fixes where you already know what is wrong
- summarising long files, logs or diffs

Keep it yourself when it needs **judgement**: architecture, design trade-offs, subtle bugs, security-sensitive code, or anything you cannot describe precisely.

## How to delegate

Call the `delegate` tool of the `ancilo` MCP server with:

- `task` – self-contained: the goal, the files involved, constraints (style, what not to touch) and how to verify (e.g. "run `cargo test -p parser`"). The local model sees nothing else of this conversation.
- `cwd` – the absolute project directory.
- `files` – the most relevant files, if you know them.
- `kind` – `tests`, `refactor`, `fix`, `docs`, `summary` or `other`.
- `allow` – `edit` (default); `shell` if it must run commands such as tests; `read` for summaries.
- `background: true` for longer work: Ancilo works on its own git branch and you continue meanwhile; fetch the result later with `task_result` (use `wait_s` to wait).

## After delegating

Check the result before relying on it: read the summary and the changed files (`task_result` with `detail: true` shows the diff), run the tests. A `failed` or `partial` status means you take over – the summary says why.
