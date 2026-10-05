# Changelog

All notable changes are listed here. Versions follow [Semantic Versioning](https://semver.org).

## Unreleased

- **Updates you can see**: when a new version is there, the header shows *Update to 0.4.5* next to *Manual* – a click shows what is new and installs it with a restart (it warns if a task is still working). While Ancilo does not look by itself, the header has *Check for updates* (a little highlighted after a month without a look). The switch for the daily look moved to a box of its own, **Updates** – in *System* and in the setup – with the version and the last look. Ancilo looks at its start too when the daily look is on. Installing still always waits for your click.
- The setup checklist no longer has *Ask something now* – the Chat tab is right there.

## 0.4.4 – 2026-10-05

- **What left this Mac** (*System*): everything Ancilo sent over the internet – web searches and the pages they read, model searches and downloads, the model list, messages to cloud models, update checks – in plain words: what, to whom, when, started by you or by Ancilo itself. A click shows each request in detail: the full address, what was sent, the answer. Kept 30 days on this Mac only, can be emptied; keys are never in it, and models (Claude Code, Codex, the assistant) cannot read it. Every request to the internet goes through one place that writes this log – a test makes sure nothing goes past it.
- **Fixed: photos and scans had no text in the app.** Text recognition worked from the command line, but inside Ancilo.app its sandbox kept macOS from reading the app itself – every picture came back empty. Found on a MacBook Air; now a receipt photo (JPEG, HEIC) and a scanned letter are read in half a second.
- **A picture without readable text is said, not guessed**: the AI tells you it cannot read it and asks for a sharper photo (before, a small model claimed to have no access, or guessed).
- **Web search keeps your words**: a small model sometimes misspelled a name in its search ("Gaustopfes" for "Gaustatoppen") or added a year you never named – now your spelling is used, invented years are dropped, and if nothing is found Ancilo searches once more with your own question.

## 0.4.3 – 2026-10-05

- **Two people, one Mac**: when someone else on the Mac runs Ancilo too, Ancilo no longer fails to start – it takes the next free port (the other one's stays untouched), and never mistakes the other person's Ancilo for its own.
- **No silent quitting**: if Ancilo's background service cannot start, the app says why (in the Mac's language) instead of disappearing.
- **Tables that open right in Excel**: a CSV a task writes is checked before it is saved – amounts like 312,50 between comma-separated columns used to split into two columns. The model is told how to write it right (quotes, or ';' between the columns). Found on a MacBook Air with Qwen3.5 4B; now 3 of 3 runs give a correct table.
- **Run from the download?** Opened straight from the disk image, Ancilo asks to be dragged into Applications first – otherwise its background service would point at a place that is gone after ejecting.

## 0.4.2 – 2026-10-05

- **Security fixes** from an independent review:
  - The coding agent's file tools no longer follow links out of the project. A broken link (to a file that does not exist yet) let `write_file` create a file outside it; `grep` and the file list read and listed files outside through links. Links inside the project work as before.
  - Ancilo's access token never reaches a model: Claude Code, Codex (MCP) and the assistant see it hidden – a cloud assistant asking for the model API settings got it. The app and the CLI still show it.
- **License notices complete**: the list now covers the native app too (Tauri and its components were missing), and Ancilo.app carries the notices itself (`Contents/Resources/licenses`).
- **Dependencies are checked** for known vulnerabilities on every change and weekly (`just audit`); the test runner is updated (vitest 4).
- **Remove Ancilo** (*System › Remove Ancilo…*, or `ancilo uninstall`): one click removes the app, its background service, the connections to Claude Code and Codex and – unless you keep them for a later install – the models Ancilo downloaded, conversations, settings and keys. Your own files, projects and the models of LM Studio or Ollama stay. Removing Ancilo Dev leaves the installed app's connection to Claude Code and Codex alone (both connect under the same names).

## 0.4.1 – 2026-10-05

- **Settings… (⌘,)** in the Ancilo menu opens System; **Help › Ancilo Manual** and *Manual* in the header open the new user manual on ancilo.app (German or English).

- **Ancilo's messages in German**: errors and failed answers – memory, loading and downloading models, web search, documents, tasks, connecting Claude Code and Codex – appear in the app's language, also in conversations from before. Ancilo keeps them in English for itself (the history a model reads stays as it was).

## 0.4.0 – 2026-10-05

- **Fixed: "the model ended without an answer"** on small reasoning models (Qwen3.5 2B on a MacBook Air: 8,192 tokens of thinking, 136 s, no answer). Chats now ask for the answer at once – seconds instead of half a minute – and any model that only thought is asked once more without thinking (chats, tasks and coding alike).
- **You see what a chat is doing**: loading the model, searching the web for “…”, writing the answer – with a clock after a few seconds, so a long wait never looks stuck. Tasks and coding show model loading and the clock too.
- **Fixed: Ancilo Dev asked for the installed app's keys** (a keychain dialog "ancilo wants to use your confidential information"). Each data directory now keeps its keys under a keychain entry of its own; the installed app keeps 'ancilo', so saved keys stay where they are.
- **A tidy menu bar**: only Ancilo, Edit and Window. The system's Services submenu (the services of every other app on the Mac), the empty File menu and Help are gone.

## 0.3.1 – 2026-10-04

- **Fixed: a loaded model was lost when its conversation grew** – "needs about 41.6 GB, but only about 33 GB are free" on a Mac with 128 GB. Ancilo counted only "free" memory, not the file cache macOS hands back at once (where a stopped model's memory lands, too). It now counts what macOS gives a program, and a model whose larger context does not start always comes back as it ran (unless memory is critically short).

## 0.3.0 – 2026-10-03

- **A proper install window**: the DMG opens a styled window – Ancilo on the left, Applications on the right, an arrow and one line in English and German between them; no toolbar, nothing hidden in sight (also with hidden files shown). Readable in light and dark mode.
- **Pictures and scans have text now**: photos of receipts (JPEG, PNG, HEIC, TIFF, WebP) and scanned PDFs are read by text recognition – the one built into macOS, on this computer, in a sandbox without network, at low priority. In chats, chat projects and tasks; recognized text is marked (it may have mistakes).
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
