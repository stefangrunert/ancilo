# The assistant

Ask Ancilo in your own words:

```bash
ancilo ask "Which models do I have and which is fastest?"
ancilo ask "Use qwen3-coder for delegated tasks"
ancilo ask "Why is the model so slow right now?"
ancilo ask "Wie viel Speicher haben meine Modelle?"
```

The assistant looks things up (documentation, model notes, your own eval results and comparisons) and reads the live state (models, memory, recent errors) through Ancilo's operations. It can do everything Ancilo can – and nothing else.

In the app there is one kind of chat. A plain chat is a conversation with the local model without any tools – fast and focused. Ancilo's tools come in when Ancilo is the topic: in a chat started with *Set up Ancilo*, or when a request is about Ancilo (models, memory, speed, setup, Claude Code). Through the API: `ask` with `"remember": true` starts a kept conversation, `"kind"` (`chat`, `setup`, `write`, `explain`, `summarize`; default `setup`) says what it is for, `"greeting"` stores the words it starts with, `"conversation": "c-…"` continues one; `list_conversations`, `get_conversation`, `rename_conversation` and `delete_conversation` manage them. A single `ask` without either is the assistant as before and leaves nothing behind.

## Confirmations

Reading is immediate. Anything that changes something – roles, rules, comparisons, A/B tests, downloads, removing models, connecting Claude Code or Codex – is **proposed**, not executed:

```
  ? assign_role model=qwen3-coder role=delegation
I proposed to use qwen3-coder for delegated tasks.
Run assign_role model=qwen3-coder role=delegation? [y/N]
```

Only the exact proposed action runs after you confirm. `--yes` confirms all proposals of one request. Starting and stopping models, pinning and indexing need no confirmation.

From other tools: operation `ask` returns `pending` actions; `confirm_action` runs one, `reject_action` drops it. Claude Code and Codex get `ask` as an MCP tool.

## First start

A fresh Ancilo has no model to think with. `ancilo ask` then proposes the first-start setup – or run it directly:

```bash
ancilo setup --dry-run   # what would be installed, and the download size
ancilo setup
```

It picks the best recommended chat model that fits your machine comfortably and a small embedding model for search. Existing models are kept; files from LM Studio, Ollama or the Hugging Face cache are reused.

## Which model answers?

The model with the role `assistant`, otherwise the default model. A small model (around 4B) is enough for most requests; `ancilo op run_assistant_eval` measures how well a model operates Ancilo.

## Cloud models

Local is the default. You can add a model of an OpenAI-compatible provider (DeepInfra, OpenRouter, Together, …):

```bash
ANCILO_API_KEY=… ancilo cloud https://api.deepinfra.com/v1/openai Qwen/Qwen3-235B-A22B-Instruct-2507
ancilo assign assistant qwen3-235b-a22b-instruct-2507
```

- The key goes into the system keychain – never into files, logs or events.
- A cloud model gets no role by itself; you assign one explicitly.
- **Code never goes to the cloud:** delegation, comparisons (and their judge), the coding agent and search embeddings refuse cloud models – checked on every model call, so a model removed meanwhile never leads to a cloud fallback. An assistant running on a cloud model can only use operations that cannot reveal code (models, roles, routes, settings, diagnosis) – not search, task or session results.
- Servers on your own machine (Ollama, LM Studio) are added with `ancilo add http://localhost:11434` and count as local.

## Diagnosis

```bash
ancilo diagnose          # hardware, models, recent errors, hints
ancilo logs <model>      # the model's llama.cpp output
```
