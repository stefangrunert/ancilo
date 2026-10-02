# The app

Like a chat app: navigation on the left, the conversation on the right.

- **New chat** – a chat with the AI on your computer, about anything: write, ask, summarize, translate. The input grows with your text: **Enter** sends, **Shift+Enter** starts a new line. Answers are formatted (Markdown). Chats are kept – continue one any time; rename or delete them in the list.
- **Quick actions** – below the input on the start page and in a new chat: *Just chat* (chosen by default), *Build something*, *Set up Ancilo*. *Set up Ancilo* starts a chat in which Ancilo helps with itself (speed, models, Claude Code) – it greets you with answers to click, and changes are proposed as plain sentences: *Yes, do it*, *Rather not* or *What does that mean?*. *Build something* and *Set up Ancilo* lead back to a new chat with one click.
- **Set up** – the start page: a step-by-step setup – what Ancilo is for, the AI that suits your computer, how much of your computer Ancilo may take, Claude Code and Codex (optional), projects and documents (optional). Each step explains what it does; afterwards a checklist where any step can be changed. A folder of documents chosen here is searchable in every chat (text files; PDF and Word not yet).
- **System** – your models (**Add** a model by its address – Ancilo checks size and fit while you type; **Context** small, medium, large), **Claude Code & Codex** (one click to connect; what delegated **Tasks may** do), and – when it matters – **Models** (roles, memory, recommendations), **Tasks** and **Comparisons**.
- **How much Ancilo may take** – the cockpit on the overview: one slider from *Eco* to *Maximum* (default *Balanced*). It decides how long a model stays loaded without use, how much memory Ancilo may use, how many requests run at once, how much priority models get, which variant (smaller or more precise) Ancilo prefers, and what happens when memory gets short or the computer hot. Below: memory and temperature right now, the loaded models with the time until they are unloaded, and *Unload all now*. 
- **Status bar** – along the bottom of the window: how the computer is doing (*All calm*, *Computer is busy*, *Computer at its limit*), memory and processor use, the temperature when it is warm, what Ancilo uses and the cockpit level; at the right end the expert-view switch and the language. When the computer gets tight, a card says why, names the biggest other programs and offers one click that helps (*Unload the AI*, *Switch to Eco*, *Open Activity Monitor*); *Later* keeps it quiet until it gets worse. In an emergency (memory or heat critical) Ancilo unloads its models by itself – except on *Maximum* – and says so, with *Load again now*. When the window is not in front, macOS shows a notification only in an emergency, at most every ten minutes.
- **Web search** – off until you turn it on under *System › Web search*: Wikipedia (no account) or Google through Serper (your own key). By default Ancilo shows the search query and asks first; answers list their sources. See [Web search](web-search.md).
- **Projects** – *Build something* (or **+**): a new project needs only a name (Ancilo creates the folder in `~/Ancilo`), or open a folder you have; each project lists its coding sessions. Type a first task on the project page to start a session – see [Coding with a local model](coding.md).

The sidebar can be hidden (and overlays the page in a narrow window). Light and dark follow the system. **Expert view** (switch at the bottom of the sidebar) shows everything else: models and roles, comparisons, tasks, permissions, fine-tuning, adding models by address and searching Hugging Face, and in coding sessions the diff, terminal, other models and permissions; the simple view keeps to plain words – *Keep* or *Undo* for changes.

## Starting it

The desktop app lives in the menu bar. It finds the Ancilo daemon – or starts it – and keeps it running after you close the window, so Claude Code and Codex can always reach Ancilo. On macOS it registers the daemon to start at login.

Without the desktop app, the same UI runs in any browser:

```bash
ancilo ui            # opens http://127.0.0.1:7424/app/ with access to your Ancilo
ancilo ui --print    # only print the address
```

The access token travels in the address fragment (`#token=…`), which browsers never send to a server.

## Updates

*Check for Updates…* in the menu bar looks for a new version; a found one is offered as *Install Ancilo … and Restart*. Updates are signed – a changed or wrongly signed update is refused – and never go back to an older version. Your models, settings and history stay. The app looks for updates on its own only if you allow it (*Look for updates automatically*).

## Languages

German and English, chosen from the system language; switch at the bottom of the sidebar.

## Everything is an operation

Every button calls one of Ancilo's operations over the local HTTP API – the same ones the CLI, MCP clients and the assistant use. Nothing exists only in the app.
