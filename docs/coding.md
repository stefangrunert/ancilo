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

Each session has one of two modes, chosen under the input:

| Mode | Without asking | Asks before |
|---|---|---|
| **Ask first** | read and search the project | every change, every command |
| **Approve for me** (default) | change files in its copy, run commands in the sandbox | – |

Either way the agent works in its copy and **only you keep changes**. There is no mode that runs commands outside the sandbox.

When the agent wants to do something its mode does not allow, it waits and shows the action: **Allow** (this once), **Allow for this session**, or **Reject** – the agent then continues without it.

### The sandbox

The agent's commands run in a sandbox (Seatbelt on macOS, Bubblewrap on Linux; without one, commands are refused):

- **no network**,
- **writing** only in the copy and the command's own temporary folder – not in `/tmp`, not anywhere else,
- **no reading** where secrets and personal data live: SSH and cloud keys, keychains, browser profiles, mail, Claude and Codex settings, and Ancilo's own data (other sessions, its key file),
- **no inherited environment**: only what tools need to be found (`PATH`, `HOME`, locale, toolchain folders) – no keys or tokens,
- at **lower priority**, so a long build does not make the computer unusable.

### Web search

Once web search is set up (System › Web search), the agent can look up documentation and error messages. The **Web search** switch beside the input decides – the same switch as in chats:

- **off** (default): every search asks first, in both modes. You see exactly the search words and where they go (Wikipedia or Google through Serper); the OK counts for this one search only.
- **on**: the agent searches without asking. Its searches show among its steps.

Only the search words go out, nothing else from the project. What comes back is marked for the agent as text from foreign pages.

Text from the web can try to steer the agent. So before you keep changes, Ancilo says when the agent read web pages in the session – and marks changed files that **run code** when the project is built, installed or tested (build scripts, package files and lock files, hooks, CI, shell scripts). Look at those before you keep them: once in your project, they run outside the sandbox.

## Models

The session uses the model with the role `coding` (or your default model); pick another one at the top. Cloud models are never used for coding sessions: code from your projects stays on your machine.

**Try again with …** repeats the last turn with another model, from exactly the same starting point, in a work area of its own. Both results appear side by side; **Use this result** takes the other one (and the conversation continues from it), **Drop** discards it. Which model's result you take is recorded as your choice in the leaderboard (`ancilo leaderboard`), shown apart from the measured results.

## Terminal

The *Terminal* tab opens a shell in the project folder. The agent's commands appear in the session's terminals, too (they run in its copy). Terminals live in Ancilo: reloading or closing the window keeps them running, reconnecting shows their recent output. Several terminals per session are possible.

## From the command line or other tools

Everything in the Code view is an operation (`ancilo op …`, REST, MCP):

```bash
ancilo op open_project '{"path": "/path/to/project"}'
ancilo op create_session '{"cwd": "/path/to/project", "permission": "read"}'
ancilo op send_message '{"session": "s-…", "text": "Add input validation to parse()", "wait": true}'
ancilo op session_diff '{"session": "s-…"}'
ancilo op apply_changes '{"session": "s-…"}' --confirm
```

Further operations: `list_projects`, `remove_project`, `approve`, `reject`, `get_session`, `list_sessions`, `discard_changes`, `retry_with_model`, `cancel_turn`, `update_session`, `delete_session`, `open_terminal`, `terminal_ticket`, `list_terminals`, `close_terminal`.

## How well does it work?

`ancilo eval coding --model <model>` runs reference tasks through exactly these operations (several with follow-up turns) and checks the result in the project – and that the project stayed untouched until the changes were applied.
