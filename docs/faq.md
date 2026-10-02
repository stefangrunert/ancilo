# FAQ

## Which models work?

Any GGUF model llama.cpp can run – from Hugging Face (`ancilo add hf.co/<org>/<repo>`), a local file, or files you already have from LM Studio, Ollama or the Hugging Face cache. Models from other OpenAI-compatible servers work too (`ancilo add http://localhost:11434`). For tool use and delegation, instruction-tuned models with tool calling (e.g. Qwen) work best; `ancilo setup` picks one that fits your machine.

## How much memory do I need?

Ancilo shows before downloading whether a model fits (`ancilo plan <address>`) and picks the largest quantization that does. On a Mac, models use unified memory; Ancilo keeps a reserve for the system and loads models on demand within a memory budget, unloading idle ones.

## Does anything leave my machine?

Not without your action – see [Privacy](../PRIVACY.md). Downloads happen when you add a model; cloud models only when you set them up, and they never receive code from your projects.

## Why delegate from Claude Code or Codex at all?

Cloud agents are strong but expensive and rate-limited. Many sub-tasks – writing tests, renames, small fixes, summaries – a local model can do. Ancilo tells the cloud agent when delegation fits and returns only a compact result, which also saves its context. See [Delegation](delegation.md).

## Is a small local model good enough?

For well-scoped tasks often yes; for open-ended work less so. Measure it on your own tasks: `ancilo eval delegation`, `ancilo eval coding`, or compare models side by side (`ancilo compare`, the app's comparisons) – the leaderboard shows what works on your machine. The reliability pipeline helps small models call tools correctly ([Model API](model-api.md)).

## Can I use it without the app?

Yes. Everything is available from the command line (`ancilo …`), the HTTP API, and MCP; the app is a view on the same operations. `ancilo ui` opens the app in a browser.

## Where are my data, and how do I remove Ancilo?

In `~/Library/Application Support/ancilo` (macOS) or the directory in `ANCILO_HOME`: models Ancilo downloaded, the database, indexes, logs. Stop the daemon (`ancilo daemon stop`), delete that directory and the app; `ancilo disconnect claude|codex` removes the connections first. Model files Ancilo reused from LM Studio, Ollama or the Hugging Face cache stay where they were.

## Something does not work

`ancilo diagnose` shows hardware, models, recent errors and hints. See [Troubleshooting](troubleshooting.md).
