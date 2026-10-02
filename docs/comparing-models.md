# Comparing models

One model for everything is the default. When you have several, Ancilo measures which one is better for what – on your tasks and your machine.

## Rules per task kind

```bash
ancilo route                      # list rules
ancilo route tests qwen3-coder    # use qwen3-coder for test-writing tasks
ancilo route tests --remove
ancilo route refactor             # which model would a refactoring use, and why
```

Kinds: `tests`, `refactor`, `fix`, `docs`, `summary`, `other`.

## Compare on one task

```bash
ancilo compare "write tests for src/parser.rs" -m qwen3-coder,devstral --check "cargo test" --repeat 3
```

Each run starts from the same commit in its own git worktree, with the same prompt, tools, reliability settings, temperature and seeds (all logged in the report). Runs are sequential so they do not compete for the GPU; load time is measured separately. Your working directory is never touched; the results stay on branches `ancilo/cmp-<id>/<label>-<n>`.

The report shows per model: success rate with a 95 % confidence interval, duration (p50/p95), load time, tokens per second, tokens, steps, reliability interventions and the size of the change. The check command runs in the sandbox (network off unless `--check-network`).

- `--blind`: models are shown as A, B, …; the mapping is revealed after `ancilo rate <id> <label>`.
- `--judge <model>`: a model rates each result 1–10 – clearly marked as model-based, never the only criterion.

## Personal benchmarks

```bash
ancilo suite save mine my-suite.yaml    # same format as the delegation eval
ancilo suite list
ancilo suite run mine -m a,b --repeat 2
```

## A/B tests in live use

```bash
ancilo ab start delegation --b devstral --share 20%
ancilo ab report delegation
ancilo ab stop delegation
```

A reproducible share of the role's real requests goes to B. For delegations the success is measured; for model-API requests latency, errors and reliability interventions. Statements need a minimum sample (30 per arm); below that the report says so. If B is clearly worse, the test stops by itself (guardrail). `--shadow`: B does every delegation additionally in its own worktree, without effect on the result.

## Leaderboard and recommendations

```bash
ancilo leaderboard [--kind tests] [--markdown]
ancilo recommendations
ancilo recommendations apply <id>
```

Recommendations appear only when the data is strong enough (at least 10 results per model and a significant difference). Nothing changes until you apply one.
