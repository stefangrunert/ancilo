# Coding with a local model

The app's **projects** come with a coding agent that runs entirely on your machine: private, offline, no API costs. Add a project, describe what you want, review the changes as a diff, apply them.

## A session

1. **Add a project** – **+** next to *Projects* in the sidebar: type its path or pick it with **Choose…** (macOS). It stays in the list; Ancilo starts indexing it for search in the background.
2. **Describe the task** on the project page – Enter starts a session with it (Shift+Enter starts a new line); **New session** starts an empty one. Each session is one conversation with the agent about the project, listed under its project. Sessions are kept: close the window, restart Ancilo – the conversation and its unapplied changes are still there.
3. **Follow the work** – answers are formatted (Markdown); the agent's steps (files read, edits, commands) are folded in between and open with a click; **Stop** ends the turn at any time.
4. **Review and apply** – after a turn a card says what changed: **Apply to project**, or **Review**: the *Changes* panel lists every changed file (`+added −removed`); click a file for its diff. **Apply all**, or select files and apply only those. **Discard** drops changes without a trace.

The agent works with your project's own path – it reads, writes and runs commands as if in the project – but everything happens in a copy of it:

The project itself changes **only when you apply**. In a git project the agent works in a separate git worktree of its own, starting from your project as it is – including uncommitted changes. If the project changed in the meantime and the changes no longer fit, applying fails and nothing is overwritten.

Without git it works the same: Ancilo keeps a snapshot outside the project and gives the agent a copy to work in (large generated folders such as `node_modules`, `target` or `.venv` are linked, not copied).

New files you have not committed yet are part of the starting point, too. When changes are applied, the ones you keep pending must still fit the project – otherwise nothing is applied and Ancilo says which changes conflict.

## What the agent may do

Each session has a permission, chosen at the top:

| Permission | Without asking | Asks before |
|---|---|---|
| read | read and search | changing files, running commands |
| edit files (default) | also change files (in its own work area) | running commands |
| edit and run commands | everything | – |

When the agent wants to do more than its permission allows, it waits and shows the action: **Allow** (this once), **Allow for this session** (raises the permission), or **Reject** – the agent then continues without it.

## Models

The session uses the model with the role `coding` (or your default model); pick another one at the top. Cloud models are never used for coding sessions: code from your projects stays on your machine.

**Try again with …** repeats the last turn with another model, from exactly the same starting point, in a work area of its own. Both results appear side by side; **Use this result** takes the other one (and the conversation continues from it), **Drop** discards it. Which model's result you take is recorded as your choice in the leaderboard (`ancilo leaderboard`), shown apart from the measured results.

## Terminal

The *Terminal* tab opens a shell in the session's copy of the project – where the agent works, so you see its files and can run tests on them. The agent's commands appear in the session's terminals, too. Terminals live in Ancilo: reloading or closing the window keeps them running, reconnecting shows their recent output. Several terminals per session are possible.

## From the command line or other tools

Everything in the Code view is an operation (`ancilo op …`, REST, MCP):

```bash
ancilo op open_project '{"path": "/path/to/project"}'
ancilo op create_session '{"cwd": "/path/to/project", "permission": "edit"}'
ancilo op send_message '{"session": "s-…", "text": "Add input validation to parse()", "wait": true}'
ancilo op session_diff '{"session": "s-…"}'
ancilo op apply_changes '{"session": "s-…"}' --confirm
```

Further operations: `list_projects`, `remove_project`, `approve`, `reject`, `get_session`, `list_sessions`, `discard_changes`, `retry_with_model`, `cancel_turn`, `update_session`, `delete_session`, `open_terminal`, `terminal_ticket`, `list_terminals`, `close_terminal`.

## How well does it work?

`ancilo eval coding --model <model>` runs reference tasks through exactly these operations (several with follow-up turns) and checks the result in the project – and that the project stayed untouched until the changes were applied.
