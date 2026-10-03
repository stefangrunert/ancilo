# Tasks – Ancilo works with your files

The **Tasks** area (the computer symbol) is for things to get done with your files: *make a table of these invoices*, *sort the photos in this folder by year*, *summarise the contracts in this folder and write a short report*.

## Starting a task

- **New task**: say what to do. Drag files onto the input (or use the paperclip) to give them to the task. Without a folder, the task gets a folder of its own (`~/Ancilo/Tasks/<title>`).
- **In a folder**: add a folder under *Folders* (**+**), then start a task in it. Ancilo works on that folder – and only on it.

## Always in a copy

Ancilo never works in your folder directly. A task starts with a **copy** of the folder (on APFS a clone that takes no extra space; Ancilo checks the space first and refuses folders with more than 20,000 files or 50 GB). Hidden files, links and files that are only in iCloud are left out – and never touched.

The agent can:

| | |
|---|---|
| look | list the files, read documents (PDF by page, Word, Excel by sheet, CSV, text), find passages in all of them |
| write | new text or CSV files, Excel files (`write_spreadsheet`: numbers stay numbers, nothing ever becomes a formula), Word documents (`write_document`) |
| sort | move and rename files, make folders, delete files |

It cannot run commands or reach the network, and no path leads out of the copy (no `..`, no absolute paths, no links). Text in documents is treated as content, never as instructions.

**Ask first** (under the input) makes the agent ask before every change; **Approve for me** lets it change the copy without asking. Either way your folder changes only when you keep the changes.

## Keep, drop, undo

When the agent is done, a card lists what changed: **new**, **changed**, **deleted**, **moved** (with where from).

- **Keep** puts the changes into your folder. Before anything is written, Ancilo checks that the folder still holds what the copy started from – if you changed a file there meanwhile, or a file of a new name appeared, **nothing** is written and Ancilo says which files. Originals are backed up first; every step is noted in a journal; files are written next to their place and moved in at once. If a step fails, everything before it is put back. If Ancilo stops in the middle (a crash), it puts things back at the next start.
- **Drop** sets the copy back to your folder – your folder is not touched.
- **Undo** (after keeping) takes the changes back out of your folder – as long as nothing changed there since; folders that were made for the changes go too.

## From the command line or other tools

```bash
ancilo op open_task_folder '{"path": "/path/to/folder"}'
ancilo op create_task '{"folder": "/path/to/folder", "title": "Invoices"}'
ancilo op send_message '{"session": "s-…", "text": "Make a table of the invoices", "wait": true}'
ancilo op session_diff '{"session": "s-…"}'
ancilo op apply_changes '{"session": "s-…"}' --confirm
ancilo op undo_apply '{"session": "s-…"}' --confirm
```

`add_task_file` puts a file into a task's copy; `discard_changes` drops changes.
