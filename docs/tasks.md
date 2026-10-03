# Tasks – Ancilo works with your files

The **Tasks** area (the computer symbol) is for things to get done with your files: *make a table of these invoices*, *sort the photos in this folder by year*, *summarise the contracts in this folder and write a short report*.

## Starting a task

Say what to do – and give Ancilo what it needs. That decides how the task works; there is nothing to choose up front:

- **Files** (drag them onto the input, or the paperclip): they are the material. Ancilo makes something new from them – a table, a summary, a letter. Your files stay as they are. At the end, **Save to Documents** – or **Somewhere else…** – puts the result where you want it (never over a file that is there: *Overview 2.xlsx*). Then **Open** it or **Show in Finder**.
- **A folder** (the folder button next to the paperclip): Ancilo works *in* it – sorts, renames, adds files. It always works in a copy: you see every change and **keep** it or not (below).

Folders Ancilo worked in show up in the Tasks area; a click starts a new task there.

## A folder: always in a copy

Ancilo never works in your folder directly. A task starts with a **copy** of the folder (on APFS a clone that takes no extra space; Ancilo checks the space first and refuses folders with more than 20,000 files or 50 GB). Hidden files, links and files that are only in iCloud are left out – and never touched.

The agent can:

| | |
|---|---|
| look | list the files, read documents (PDF by page, Word, Excel by sheet, CSV, text), find passages in all of them |
| write | new text files (`.txt`, `.md`, `.csv` and other plain text – nothing that runs when opened), Excel files (`write_spreadsheet`: numbers stay numbers, nothing ever becomes a formula), Word documents (`write_document`) |
| sort | move and rename files (a file keeps its ending), make folders, delete files |

It cannot run commands or reach the network, and no path leads out of the copy (no `..`, no absolute paths, no links). Text in documents is treated as content, never as instructions.

**Ask first** (under the input) makes the agent ask before every change; **Approve for me** lets it change the copy without asking. Either way your folder changes only when you keep the changes.

## Keep, drop, undo

When the agent is done, a card lists what changed: **new**, **changed**, **deleted**, **moved** (with where from).

- **Keep** puts the changes into your folder. Before anything is written, Ancilo checks that the folder still holds what the copy started from – if you changed a file there meanwhile, or a file of a new name appeared, **nothing** is written and Ancilo says which files. Originals are backed up first; every step is noted in a journal; files are written next to their place and moved in at once. If a step fails, everything before it is put back. If Ancilo stops in the middle (a crash), it puts things back at the next start.
- **Drop** sets the copy back to your folder – your folder is not touched.
- **Undo** (after keeping) takes the changes back out of your folder – as long as nothing changed there since; folders that were made for the changes go too.

## From the command line or other tools

```bash
ancilo op create_task '{"folder": "/path/to/folder", "title": "Invoices"}'
ancilo op send_message '{"session": "s-…", "text": "Make a table of the invoices", "wait": true}'
ancilo op session_diff '{"session": "s-…"}'
ancilo op apply_changes '{"session": "s-…"}' --confirm
ancilo op undo_apply '{"session": "s-…"}' --confirm
```

Without `folder`, `create_task` starts a task with files: `add_task_file` gives it a file, `save_results` (`dir`, default Documents) saves its results. `discard_changes` drops a folder task's changes.
