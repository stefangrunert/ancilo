# Model API

Ancilo serves loaded models under one address, `http://127.0.0.1:7424`, with the token from the data directory (`Authorization: Bearer …` or `x-api-key: …`).

| Endpoint | Compatible with |
|---|---|
| `POST /v1/chat/completions` | OpenAI Chat Completions (streaming, tools) |
| `POST /v1/responses` | OpenAI Responses API (Codex) |
| `POST /v1/messages`, `/v1/messages/count_tokens` | Anthropic Messages (Claude Code) |
| `POST /v1/embeddings` | OpenAI embeddings (role `embed`) |
| `GET /v1/models` | model ids and roles |

The `model` field may be a model id, a role (`default`, `delegation`, …) or any other name – unknown names use the default model, so clients that insist on their own model names just work. Map names explicitly with `model_aliases` in `config.toml`.

## Starting Claude Code or Codex on the local model

```bash
ancilo claude     # Claude Code with its own configuration directory
ancilo codex      # Codex with an "ancilo" provider
```

`ancilo claude` uses a separate `CLAUDE_CONFIG_DIR`: a logged-in Claude Code would otherwise send its Anthropic credentials to any base URL. Your credentials never reach Ancilo.

## Reliability pipeline

Small local models make tool-calling mistakes. For requests with tools, Ancilo runs a pipeline of stages that can be switched individually:

| Stage | What it does |
|---|---|
| `prompt` | adds a short instruction on when and how to use tools |
| `constrained` | lets llama.cpp enforce the tool-call format with a grammar |
| `validate` | checks tool calls against the tool schemas |
| `repair` | fixes broken JSON and near-miss tool names |
| `retry` | asks again after an invalid call |
| `nudge` | when a request asked for an action on the project but the model only answered in text, asks once more |

Default: `standard` – every stage except `prompt`. The prompt hint helps some models and hurts others (small models start calling tools for plain questions), so it is enabled per model by measurement:

```bash
ancilo op tune_reliability '{"model": "<model>"}'
```

measures the candidates on two eval sets (with and without tools needed) and keeps the best pipeline that causes no more unwanted tool calls than no pipeline at all. Per request: header `x-ancilo-reliability: off`, `all` or a list such as `constrained,validate`. Globally: `ancilo op set_reliability '{"stages": "all"}'`; per model: add `"model": "<id>"` (`"stages": "default"` removes it). `ancilo op gateway_stats` shows how often each stage stepped in.

Requests have a priority (header `x-ancilo-priority`: `interactive`, `sync`, `background`, `compare`); interactive work goes first.

## Evals

```bash
ancilo eval tool-calling --model <model> --reliability off
ancilo eval no-tool --model <model>          # must NOT call tools
ancilo eval delegation --model <model>       # real tasks on fixture repositories
```
