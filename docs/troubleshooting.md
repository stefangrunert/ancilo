# Troubleshooting

## "does not fit"

The model needs more memory than is usable for models. Try a smaller quantization (`--quant Q4_K_M`), a smaller context (`--context small`), a smaller model, or lower the reserve with `ram_reserve_gib` in `config.toml`. `ancilo hardware` shows the numbers; `ancilo plan <address>` shows what a model would need.

## "Stop another model first"

The model fits on this machine, but not next to the models loaded right now. Stop one (`ancilo stop <model>`) – or just send a request: models are loaded on demand and the least recently used idle model is unloaded automatically.

## "not enough free memory right now"

Your other programs use the memory a model needs. Close some of them, choose a smaller model, or – if you accept that other programs slow down – set Ancilo to *Maximum* in the cockpit (`ancilo op set_resources '{"level": "max"}'`).

## My computer gets slow or hot

Open the cockpit (overview): it shows what Ancilo takes right now. *Unload all now* frees it at once; *Eco* keeps Ancilo small and quiet. Answering itself keeps the graphics chip busy for its duration – a smaller model finishes sooner.

## Downloads

Downloads resume after interruptions. If Hugging Face asks for a login for a gated model, accept its license on the website and set `HF_TOKEN`. Files from LM Studio, Ollama and the Hugging Face cache are reused instead of downloaded again.

## The model answers in text instead of calling a tool

Small models sometimes describe an action instead of doing it. The reliability pipeline (stage `nudge`) asks once more in that case. If it still happens often, use a larger model for the role (`ancilo assign delegation <model>`), or compare models with `ancilo compare`.

## A request is slow the first time

The model is loaded on the first request (seconds to a minute for large models). Streaming clients receive keep-alive comments while it loads. Pin frequently used models so they stay loaded: `ancilo op set_pinned '{"model": "…", "pinned": true}'`.

## Claude Code asks me to log in / uses my account

`ancilo claude` starts Claude Code with its own configuration directory so that your Anthropic credentials are never sent to Ancilo. That separate configuration is not logged in – that is intended. `ancilo connect claude` (delegation via MCP) uses your normal Claude Code.

## Shell commands are refused

Commands of the local agent run only in a sandbox. On Linux install Bubblewrap (`bwrap`).

## Where are logs?

`<data directory>/logs` holds the logs of model processes. `ancilo events` follows everything Ancilo does, live.

## "llama.cpp is not installed"

Packages (app, Homebrew, release archive) contain llama.cpp. A build from source has none: install the pinned build once with

```bash
ancilo llama install
```

If Ancilo reports that the installed llama.cpp "was changed or damaged", the same command reinstalls it. To use your own llama.cpp build instead, set `llama_server_bin` in `config.toml`.

