# Privacy

Ancilo is built to keep your work on your machine.

## What stays local

- **Models run locally** (llama.cpp). Prompts, code, answers, embeddings and indexes never leave the machine for local models.
- **The daemon listens only on `127.0.0.1`** and requires a token (stored with permissions 0600). Requests from other hosts or foreign web origins are rejected.
- **Your data** – the model library, settings, task and session history, comparison results, indexes – lives in Ancilo's data directory (`~/Library/Application Support/ancilo` on macOS, or `ANCILO_HOME`).
- **No telemetry.** Ancilo sends no usage data, crash reports or analytics.

## What goes out – and only when you ask for it

| When | To | What |
|---|---|---|
| You add a model from Hugging Face | `huggingface.co` | the model's name; the files and its model card (README) are downloaded |
| You rebuild the knowledge base (`ancilo knowledge`, `refresh_model_knowledge`) | `huggingface.co` | requests for the model cards of your installed Hugging Face models. Starting Ancilo does not do this – it only re-reads the cards already downloaded. |
| You open the model choice in the app (or call `recommend_models` with `refresh`) | the Ancilo repository on GitHub | a request for the newest model list; nothing about you, your computer or your data – the comparison with your computer happens locally |
| You search for a model (`search_models`) | `huggingface.co` | the words you searched for |
| You add a model from another server (`ancilo add http://…`) | that server | requests to the model you chose |
| You turn on web search with **Wikipedia** (System › Web search) and a chat or a coding agent searches | `wikipedia.org` | the short search query and the article titles Ancilo looks up – not your question, not the conversation. By default Ancilo shows the query and asks before each search. |
| You turn on web search with **Google through Serper** (your own key) and a chat or a coding agent searches | `google.serper.dev` (Serper passes the query on to Google) | the short search query and your Serper key. When Google shows no direct answer, **this computer opens up to three result pages** – those websites see your IP address, like when you browse (no cookies, no referrer, nothing loaded from them besides the page). |
| You set up a cloud model and use it | that provider (e.g. DeepInfra, OpenRouter) | the requests you send to that model. **Code from your projects is never sent to cloud models** – delegation, coding sessions and comparisons only use local models. |
| You click *Check for Updates…* in the app's menu – or, only if you allowed it in the app, once a day | the release server (GitHub) | a request for the latest version; nothing about you or your data |

API keys for cloud providers and the Serper key are stored in the system keychain, never in files or logs. Web search and your documents only ever go to the AI on this computer – never to cloud models.

**Chat projects and tasks** work the same way: documents are read on this computer in that separate process, a chat project's text is kept in Ancilo's database (and forgotten when you take the project off the list), a task works in a copy of the folder in Ancilo's data folder. Nothing of it goes anywhere else.

**Documents you attach to a chat** are read on this computer, in a separate process without network access; Ancilo keeps only their text (in its database, deleted with the conversation), never a copy of the file. A conversation with a document stays with the AI on this computer for good: no cloud model – not even for its earlier messages – and every web search asks first.

## Claude Code and Codex

When you connect Claude Code or Codex, they call Ancilo locally (MCP). `ancilo claude` starts Claude Code against the local model with a separate configuration directory, so your Claude account credentials are never sent to Ancilo.
