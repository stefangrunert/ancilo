# Web search

The chat can look things up on the web – when you want it to. It is **off** until you turn it on under **System › Web search** (or *Set up web search* in a chat).

## Where Ancilo searches

| | Account | Good for |
|---|---|---|
| **Wikipedia** | none, free | facts about places, people, history, science – not today's news |
| **Google (through Serper)** | your own, free for 2,500 searches, then about 1 US dollar per 1,000 | everything, also current events |

### Setting up Serper

1. Open [serper.dev](https://serper.dev/) and sign up (with Google or an e-mail address) – no credit card needed.
2. The dashboard shows your **API key**. Copy it.
3. Paste it on Ancilo's *Web search* page and click **Check and save**. Ancilo tries one search first and saves the key only if it works – in your computer's keychain, never in a file.

Ancilo never pays anything by itself; when your free searches are used up, Ancilo says so.

## When Ancilo searches

- **Ask me first** (default): when a question needs facts, Ancilo shows the search query – change it if you like – and searches only after you click **Search**. *Answer without the web* answers from what the AI knows.
- **Automatically**: Ancilo searches by itself when a question needs current facts.
- The **globe** next to the input (*Search the web*) searches for your next message in any case.

Writing, translating, explanations and small talk are not searched.

## What goes out

- Only a **short search query** goes to the provider – not your whole question, not the conversation.
- With Serper, when Google shows no direct answer, your computer opens up to three result pages, like your browser does: those websites see your internet address. Pages are only fetched from the public internet – never from your computer or your home network.
- Reading the pages and answering stays with the AI on your computer. Web search never runs with a cloud model.

Every answer with a web search says what was searched and lists its **sources**; `[1]` in the answer links to source 1.

## For experts

`ancilo op get_web_search`, `set_web_search` (`provider`: `off`, `wikipedia`, `serper`; `mode`: `ask`, `auto`; `serper_key`), `test_web_search`, `web_search` (`query`, `topic`, `lang`) and `answer_web_proposal`. Why it works this way, the measurements with a 4-billion-parameter model and the security review: decision `2026-10-02-websuche`.
