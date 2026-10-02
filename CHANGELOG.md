# Changelog

All notable changes are listed here. Versions follow [Semantic Versioning](https://semver.org).

## Unreleased

- Local models with llama.cpp: add from Hugging Face, local files (LM Studio, Ollama, Hugging Face cache) or other servers; hardware-aware quantization and context; load on demand within a memory budget
- Model API: OpenAI Chat Completions and Responses, Anthropic Messages, embeddings; reliability pipeline for tool calls, tuned per model
- Delegation from Claude Code and Codex via MCP (synchronous or in the background on a branch), sandboxed commands
- Several models: roles, routing per task kind, comparisons, blind rating, A/B tests, leaderboard, recommendations
- Hybrid code search and a knowledge base
- Assistant with confirmation of changes; cloud models (keys in the keychain, never code)
- Desktop app (Tauri) with the UI served by the daemon; Code view: coding sessions with approvals, diff review, terminals, retry with another model
