# Ancilo

**Local AI made simple – a private chat on your own computer, a coding agent, a model API for any tool, and a worker that Claude Code and Codex hand tasks to.**

🌐 **[ancilo.app](https://ancilo.app)**

Ancilo runs open models (e.g. Qwen) on your Mac with llama.cpp. It picks a model that fits your computer, downloads it, starts it – and makes sure it never makes your computer unusable. Everything stays on your machine unless you explicitly turn on web search or set up a cloud model.

## For everyone

- **Set up in a few clicks** – a step-by-step setup asks what you want to use Ancilo for and recommends one model that fits your computer's memory right now. One click loads it.
- **A private chat** – write, ask, translate, summarize; answers in Markdown; chats are kept.
- **Web search, if you want it** – off by default. Wikipedia (no account) or Google through Serper (your own key); by default Ancilo shows the search query and asks before anything goes out, and answers name their sources.
- **Your computer stays usable** – a slider from *Eco* to *Maximum* decides how much Ancilo may take. Models load only into memory that is free, idle models are unloaded, and Ancilo backs off when memory gets short or the computer hot. A status bar shows how the computer is doing and offers one-click help when it gets tight.
- **Your documents** – attach PDF, Word or Excel files to a chat, or make a folder a chat project: answers name the file and page, and such a chat stays on your computer.
- **Tasks** – give Ancilo something to do with your files: a table of your invoices, photos sorted by year, a report from your contracts. It works in a copy of the folder; you see every change and keep it, drop it or undo it.
- **Build something** – describe what you want; a coding agent works in a copy of the project, and you keep or undo its changes.

## For developers

- **Model API** – OpenAI- (`/v1/chat/completions`, `/v1/responses`) and Anthropic-compatible (`/v1/messages`) on `127.0.0.1`, with a reliability pipeline that makes small models call tools correctly.
- **Delegation** – Claude Code and Codex hand self-contained sub-tasks (tests, renames, small fixes) to your local model through MCP: cheaper, private, not rate-limited.
- **Coding agent** – chat with the agent about a project, approve what it may do, review diffs, apply; an integrated terminal; "try again with another model".
- **Several models** – roles, routing per task kind, side-by-side comparisons, A/B tests in live use, a leaderboard from your own results.
- **Search** – a hybrid code index and a knowledge base the agents and the assistant use.
- **API first** – every function is an operation, available over REST, the CLI and MCP alike.

> **Status: early, released.** Signed and notarized downloads for macOS on Apple Silicon; the app updates itself (with your click). The CLI and daemon also run on Linux – build them from source.

## Install

Download **[Ancilo.dmg](https://github.com/stefangrunert/ancilo/releases/latest/download/Ancilo.dmg)** (macOS 13 or later, Apple Silicon), open it and drag Ancilo into Applications. Open Ancilo – the setup takes it from there. All versions: [Releases](https://github.com/stefangrunert/ancilo/releases) · [Changelog](CHANGELOG.md).

## Install from source

Requirements: macOS on Apple Silicon, Rust (see `rust-toolchain.toml`), Node 22+, [`just`](https://just.systems), `cargo-nextest`.

```bash
git clone https://github.com/stefangrunert/ancilo.git
cd ancilo
just app-install  # builds Ancilo.app, installs it to /Applications, links `ancilo` into ~/.local/bin
```

Open Ancilo from Applications – the setup takes it from there.

## Quick start

The same from the terminal:

```bash
ancilo setup                              # a chat model that fits this machine + an embedding model
ancilo add hf.co/unsloth/Qwen3-4B-GGUF    # or add a model of your choice
ancilo connect claude                     # let Claude Code delegate to Ancilo
ancilo ui                                 # open the app in the browser
```

## Documentation

- [Getting started](docs/getting-started.md) · [Models](docs/models.md) · [The app](docs/app.md) · [Coding with a local model](docs/coding.md)
- [Delegation from Claude Code and Codex](docs/delegation.md) · [Model API](docs/model-api.md) · [Comparing models](docs/comparing-models.md)
- [Search](docs/search.md) · [Web search](docs/web-search.md) · [Assistant](docs/assistant.md) · [Troubleshooting](docs/troubleshooting.md) · [FAQ](docs/faq.md) · [Eval results](docs/evals.md) · [Where the code comes from](docs/provenance.md)
- [Privacy](PRIVACY.md) · [Security](SECURITY.md) · [Contributing](CONTRIBUTING.md) · [Changelog](CHANGELOG.md)
- Website: [ancilo.app](https://ancilo.app)

## Development

```bash
just build        # release build of the `ancilo` binary
just verify       # formatting, lints, all deterministic tests, app checks
just app-e2e      # the app in Chromium and WebKit against a real daemon with fakes
just test-real    # product checks with real small models (~10–15 min; downloads them once)
just bench-reliability  # the reliability benchmark (~45 min) – when working on the pipeline
just release      # build, sign, notarize and draft a release (maintainers)
```

`just verify` runs offline and without a GPU: every external dependency – Hugging Face, models, web search providers, Claude Code and Codex – has a deterministic fake. How the test tiers and releases fit together: [Contributing](CONTRIBUTING.md).

## License

[Apache-2.0](LICENSE)
