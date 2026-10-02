# Getting started

Ancilo runs local language models on your Mac (Apple Silicon) or Linux machine and makes them useful in three ways:

1. as a **model API** (OpenAI- and Anthropic-compatible) for any tool,
2. as a **worker** that Claude Code and Codex delegate tasks to,
3. as your own **coding agent** (`ancilo run "…"`).

## Install and add a first model

**In the app:** the first start asks what you want to use Ancilo for (chat and writing, programming, your own documents), looks at your computer – its memory, what other programs use right now, how fast its chip is – and recommends the model that fits, with two or three alternatives. One click downloads and starts it. Later: *Overview → Find another model*. The same recommendation from the command line: `ancilo op recommend_models '{"purposes": ["chat"]}'`.

**From the command line:**

```bash
ancilo add hf.co/unsloth/Qwen3-4B-GGUF
```

That is all. Ancilo

- checks your hardware and picks the best quantization that fits into memory,
- downloads the model (resumable; files you already have from LM Studio, Ollama or the Hugging Face cache are reused),
- starts the model with the `llama.cpp` that comes with Ancilo (built from source without it? `ancilo llama install` downloads the pinned, checksum-verified build).

The first model you add becomes the **default model** and serves everything until you assign other models to roles.

See what would happen before downloading:

```bash
ancilo plan hf.co/unsloth/Qwen3-4B-GGUF
```

## Everyday commands

| Command | What it does |
|---|---|
| `ancilo list` | models, their roles and status |
| `ancilo status <model>` | one model in detail |
| `ancilo start <model>` / `ancilo stop <model>` | load into / unload from memory |
| `ancilo remove <model>` | remove (deletes files Ancilo downloaded) |
| `ancilo hardware` | memory available for models |
| `ancilo run "<task>"` | let the local model do a task in the current project |
| `ancilo connect claude` / `ancilo connect codex` | let Claude Code / Codex delegate to Ancilo |
| `ancilo ask "<request>"` | the assistant: ask or instruct Ancilo in your own words |
| `ancilo setup` | first start: a chat model that fits this machine and an embedding model |
| `ancilo diagnose` | hardware, models, recent errors, hints |
| `ancilo search "<question>"` | search the project (or `--knowledge`: Ancilo's docs and model notes) |
| `ancilo ops` | every operation; `ancilo op <name> '<json>'` calls one |

Add `--json` to any command for machine-readable output.

## The daemon

Commands talk to a small background service (the daemon) on `127.0.0.1:7424`. It starts automatically when needed. `ancilo daemon status|start|stop` manages it explicitly. It only listens on the loopback interface and requires a token (stored in the data directory with permissions 0600).

Data directory: `~/Library/Application Support/ancilo` on macOS, `~/.local/share/ancilo` on Linux. Override with `ANCILO_HOME`.
