# Search and knowledge

Local models have small context windows. Instead of reading a whole project, they search it.

## Project search

```bash
ancilo search "where is the token checked"     # in the current project
ancilo search parse_config --limit 3
ancilo index            # index the project now (otherwise it happens on first use)
ancilo index --status
ancilo index --remove
```

The search combines meaning (embeddings) and exact words (full text) and puts exact symbol names first. Code is split into functions, classes and types (Rust, Python, JavaScript, TypeScript, Go, Java), Markdown into sections, everything else into overlapping windows. `.gitignore`, hidden files, binaries and files over 1 MB are left out.

The index stays current by itself: before each search, files whose size or modification time changed are processed again – nothing else. A delegated task starts indexing its project in the background, and the worker gets a `search` tool. Claude Code and Codex get `search` as an MCP tool.

Semantic search needs an embedding model:

```bash
ancilo add hf.co/second-state/All-MiniLM-L6-v2-Embedding-GGUF
```

It takes the `embed` role automatically. Without one, search is full text only.

Indexes live in the data directory (`index/`), not in your project.

## Knowledge base

Ancilo also indexes knowledge about itself and about models: this documentation, curated notes on recommended models, the model cards of your installed models, your eval results and the comparison leaderboard.

```bash
ancilo search --knowledge "which quantization should I choose"
ancilo knowledge            # rebuild (fetches model cards from Hugging Face)
ancilo knowledge --status
```

It is rebuilt when Ancilo starts and follows new eval results, comparisons and added models automatically. A model's card is downloaded once when you add the model; `ancilo knowledge` downloads all cards again. Starting Ancilo never contacts Hugging Face – it uses the cards already on disk.

## Measuring search quality

```bash
ancilo op run_retrieval_eval '{"suite": "retrieval"}'   # code: recall@5, MRR, latency
ancilo op run_retrieval_eval '{"suite": "knowledge"}'
```
