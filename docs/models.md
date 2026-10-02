# Models

## Addresses

`ancilo add` accepts:

- a Hugging Face repository: `hf.co/unsloth/Qwen3-4B-GGUF`, optionally with a quantization: `hf.co/unsloth/Qwen3-4B-GGUF:Q4_K_M`
- a local GGUF file: `/path/to/model.gguf`
- an OpenAI-compatible server: `http://localhost:11434` (Ollama), `http://localhost:1234` (LM Studio) – add `--model <name>` if it serves several; cloud providers: see [the assistant](assistant.md#cloud-models)

## Context size

`--context small|medium|large` (8k, 32k, 128k tokens; default medium). A larger context needs more memory for the KV cache. If a request does not fit, Ancilo restarts the model with a larger context automatically (up to what the model and memory allow) – agents like Codex and Claude Code send large prompts.

## Memory and quantization

Ancilo computes how much memory is usable for models: on Apple Silicon, the GPU may use about ¾ of RAM on machines with more than 36 GB and about ⅔ below; a reserve (20 % of RAM, at most 8 GB) stays free for the system and other apps. Set `ram_reserve_gib` in `config.toml` to change the reserve.

From the files of a repository it picks the best quantization that fits (at most ~8.6 bits per weight – more brings no measurable quality). The verdict is one of:

- **fits** – runs comfortably,
- **tight** – runs, but leaves little room (other apps may be squeezed),
- **does not fit** – choose a smaller model or quantization.

Several models can be loaded at once as long as they fit. When a request needs a model that does not fit next to the loaded ones, the least recently used idle model is unloaded first ("load on demand"). Models in use and pinned models are never unloaded.

## Roles

A role names a purpose; each role is served by one model:

| Role | Used for |
|---|---|
| `default` | everything without a more specific role (the first model you add) |
| `delegation` | tasks from Claude Code, Codex and `ancilo run` |
| `coding` | the coding agent in the app |
| `assistant` | the Ancilo assistant |
| `embed` | embeddings for search (set automatically for embedding models) |

```bash
ancilo assign delegation qwen3-coder-30b-a3b-q4_k_m
```

Roles without a model fall back to the default model. With one model you never need to think about roles.

## Your computer comes first

Ancilo never makes your computer unusable – also on an ordinary 8 or 16 GB laptop:

- A model is loaded only into memory your computer has **free right now** (what your other programs leave). If there is no room, Ancilo says so instead of pushing your computer into swapping.
- A model that is not used gives its memory back (after 15 minutes by default) and is loaded again with the next request.
- Nothing is loaded in advance when you log in – unless you chose to keep models loaded.
- Models run at a lower priority than your programs (about 4 % slower; *Eco*: about 25 %).
- When memory gets short or your computer hot, Ancilo unloads models that are not answering.

One setting decides how much Ancilo may take: `ancilo op set_resources '{"level": "eco"}'` (`eco`, `balanced` – the default, `performance`, `max`), or single values (`keep_loaded_secs`, `keep_loaded_always`, `max_share`, `parallel`, `priority`, `variant`, `guard`). `ancilo op resource_status` shows the settings, memory and temperature right now and the loaded models; `ancilo op unload_models` frees everything that is not answering. `ancilo op system_health` says whether the computer is getting tight, why, which programs take the most and what helps. In the app: the cockpit on the System page and the status bar. Only *Maximum* takes the risk of slowing other programs down.

## Which model?

Ancilo keeps a curated list of models that run well locally (`knowledge/catalog.json`) and recommends from it what suits *this* computer: `recommend_models` (in the app: the model choice) weighs the memory models may use, what other programs leave free right now, the chip's speed and what you want to do (`chat`, `code`, `documents`). Each suggestion says how good it is (Ancilo's estimate, 1–10; *tested* where Ancilo's own evals ran), how fast it will answer (estimated, or measured once installed), the download and memory it needs, and whether it fits comfortably or only with other programs closed. Its `address` goes straight into `add_model`. A newer list is fetched from the Ancilo repository only when asked (`refresh`); `search_models` searches Hugging Face for anything else.


- **Small (0.6–4B):** fast, fine for simple edits and tool calls; weak at multi-step work.
- **Medium (7–14B):** good all-rounders for delegation.
- **Mixture-of-experts (e.g. 30B-A3B, 35B-A3B):** quality of a large model at the speed of a small one, if you have the memory (≈ 20–40 GB).

Measure instead of guessing: `ancilo compare` and `ancilo suite run` compare models on your own tasks (see [Comparing models](comparing-models.md)).
