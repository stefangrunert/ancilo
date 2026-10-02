# Delegation: Claude Code and Codex hand work to Ancilo

Cloud agents are strong but expensive and rate-limited. Many sub-tasks – writing tests, renaming, small fixes, summaries – a local model can do. Ancilo exposes itself to Claude Code and Codex as an MCP server with a `delegate` tool.

## Connect

```bash
ancilo connect claude    # installs the Ancilo plugin (MCP server, skill, worker agent)
ancilo connect codex     # registers the MCP server and adds a short AGENTS.md note
ancilo disconnect claude|codex
```

Codex asks before each call of an Ancilo tool. In non-interactive runs (`codex exec`) it cannot ask – allow Ancilo's tools up front there:

```bash
codex exec -c mcp_servers.ancilo.default_tools_approval_mode='"approve"' "…"
```

What delegated tasks may do is limited by Ancilo itself (`Tasks may` in the app, `set_permissions`).

Try the Claude Code plugin for one session without installing it:

```bash
claude --plugin-dir "$(ancilo claude-plugin)"
```

## How delegation works

`delegate` takes a concrete, self-contained task and the project directory. The local model works with file tools (read, search, edit, write) and optionally a shell, then returns a compact result: summary, changed files, diff stat.

- **synchronous** (default): works directly in the project; the caller waits.
- **background** (`background: true`): works in its own git worktree on the branch `ancilo/<id>` and commits there; the caller gets a task id at once and merges when it wants (`git merge ancilo/<id>`). Nothing interferes with work going on in the project meanwhile.

Permissions (`allow`):

| Level | May |
|---|---|
| `read` | read and search only |
| `edit` (default) | also change and create files inside the project |
| `shell` | also run commands – in a sandbox: writing only inside the project and temporary directories, no network |

The sandbox uses Seatbelt on macOS and Bubblewrap on Linux; without a sandbox shell commands are refused.

## From the command line

```bash
ancilo run "write unit tests for src/parser.rs"
ancilo run --background "rename Config to Settings"
ancilo tasks
ancilo task <id> --diff --wait
ancilo cancel <id>
```

## Which model does the work?

In this order: an explicit model in the request > a rule for the task kind (`ancilo route tests <model>`) > the `delegation` role > the default model. The task kind (tests, refactor, fix, docs, summary, other) is given by the caller or estimated from the task text. `ancilo route <kind>` shows which model a kind would use and why.
