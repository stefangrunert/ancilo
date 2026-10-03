# Changelog

All notable changes are listed here. Versions follow [Semantic Versioning](https://semver.org).

## Unreleased

- **Fixed: connecting Claude Code or Codex from the app** failed with "cannot run claude: No such file or directory" – an app starts with a bare search path. Ancilo now finds them where your terminal does (Homebrew, npm, `~/.local/bin`, nvm …), with the `node` npm tools need; if one is not installed, it says so and where to get it.

## 0.2.0 – 2026-10-03

- **Tasks**: first choose a folder (the dialog opens in Documents), then say what to do – or start from an example. Ancilo works in a copy that holds only what the task changes – a task starts at once, whatever the folder’s size (Documents too); it reads the folder as it is and never writes there. Files dragged onto the input go into the copy – or, if the folder already holds the same file, that one is used; the agent is told which files you gave. You see every change (new, changed, deleted, moved) and keep it, drop it or undo it. Keeping checks the folder first (nothing is written if it changed meanwhile), backs up originals, writes with a journal and rolls back on failure or after a crash. Nothing to set: a task works on its own in its copy; web searches follow the one web search switch, as in chats and coding.
- **You see the agent work**: what it says on the way ("Let me look at the PDFs …") appears at once, with the steps so far – in tasks and coding, in both views.
- **Chat projects**: folders Ancilo only reads; chats in them answer from their documents with file and page, and the project page says what could not be read and why. Plain chats no longer see document folders.
- **Documents in chats**: attach PDF, Word, Excel, CSV or text files (paperclip, drag and drop, paste). They are read on this computer in a sandboxed process; answers name the file and page or sheet. A conversation with documents stays local – no cloud model, no tools.
- **Three areas: Chat, Tasks, Code** – tabs with symbols at the top of the left column. *Set up* and *System* moved into the header, which in the app is the window's title bar. The left column can be resized and remembers its width. *Expert view* and the language sit at the right end of the header.
- **Coding Tasks**: what Claude Code and Codex hand to Ancilo is now called *Coding Tasks* (in every language) and lives in the Code area, together with what such tasks may do.
- **A model that works keeps working.** When a conversation outgrows the model's context, Ancilo restarts it with a larger one only if that fits right now – counting the memory the model frees itself, and waiting until the system really reports it free. If the restart fails anyway, the model comes back as it was. Before, the model could be gone with "only … GB are free".
- **The coding agent shortens older tool output** when its conversation no longer fits the context, instead of failing.
- **The model an agent works with stays loaded for the whole turn** – also while its commands run: no unloading for being idle, for tight memory or for another model. Only an emergency (critical memory or heat) still unloads it.
- **Coding: two access modes** for every session – *Confirm each step* or *Work on its own* (new default: changes in the copy and sandboxed commands without asking). No mode runs commands outside the sandbox.
- **One web search switch** beside the input, for everything – chats (with documents too), tasks and coding: off – Ancilo asks before every search; on – it searches without asking. It replaces the globe that searched only the next message.
- **Coding: web search** for the agent, once web search is set up – with the switch off, every search shows its words and asks first.
- **Coding: a stricter sandbox** – no `/tmp`, a temporary folder of its own, no reading of SSH/cloud keys, keychains, browser profiles or Ancilo's own data, no inherited environment, lower priority.
- **Coding: before keeping**, files that run code on build or install are marked, and sessions that read web pages say so.
- **Coding: new projects** – name, then the folder it goes into; **+** next to a project starts another chat; terminals open in the project folder.
- **Coding: a turn whose model fails says why** instead of seeming to do nothing.
- For developers: `just app-dev` installs *Ancilo Dev* next to the release (own data, port 7425, no updates).

## 0.1.0 – 2026-10-02 (pre-release)

The first public version of Ancilo – for macOS on Apple Silicon (13 Ventura or newer), signed and notarized.

**For everyone**

- Step-by-step setup: what Ancilo is for, one recommended model that fits your computer's memory right now (one click loads it), how much of the computer Ancilo may take, Claude Code and Codex, projects and documents.
- A private chat with the AI on your computer – Markdown answers, kept and renamable chats, quick actions; it can draw on a folder of your documents.
- Web search, off by default: Wikipedia (no account) or Google through Serper (your own key). By default Ancilo shows the search query and asks first; answers name their sources.
- Your computer stays usable: one slider from *Eco* to *Maximum*; models load only into free memory, idle models are unloaded, Ancilo backs off when memory gets short or the computer hot. A status bar shows how the computer is doing and offers one-click help when it gets tight; in an emergency Ancilo makes room by itself and says so.
- Build something: a coding agent works on a copy of your project; keep or undo its changes. Projects and their sessions can be renamed and sorted by hand.
- Simple and expert view; English and German.

**For developers**

- Local models with llama.cpp: from Hugging Face, local files (LM Studio, Ollama, Hugging Face cache) or other servers; hardware-aware quantization and context; loading on demand within a memory budget.
- Model API on `127.0.0.1`: OpenAI Chat Completions and Responses, Anthropic Messages, embeddings; a reliability pipeline for tool calls, tuned per model.
- Delegation from Claude Code and Codex via MCP (synchronous or in the background on a branch), sandboxed commands.
- Several models: roles, routing per task kind, comparisons with blind rating, A/B tests, leaderboard, recommendations.
- Hybrid code search and a knowledge base; an assistant that proposes changes and waits for confirmation; cloud models optional (keys in the keychain, never code).
- Everything is an operation – REST, CLI and MCP alike.

**Install:** download the DMG, open it and drag Ancilo to Applications. The CLI archive (`ancilo-0.1.0-aarch64-apple-darwin.tar.gz`) contains `ancilo` and llama.cpp for terminal use.
