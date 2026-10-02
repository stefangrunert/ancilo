# Changelog

All notable changes are listed here. Versions follow [Semantic Versioning](https://semver.org).

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
