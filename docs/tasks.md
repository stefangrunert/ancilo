# Tasks – Ancilo works with your files

The **Tasks** area (the computer symbol) is for things to get done with your files: *make a table of these invoices*, *sort the photos in this folder by year*, *summarise the contracts in this folder and write a short report*.

## Starting a task

A new task has two steps:

1. **Folder** – *Choose a folder* opens the system's dialog, in Documents. Ancilo works in a copy of it: only what you keep goes into the folder (below). On a folder's own page the folder is set; on *New task*, **Change** picks another one.
2. **What should Ancilo do?** – say it in your own words, or start from an example (*For example:* – a click puts it into the input). Files dragged onto the input (or the paperclip) go into the copy too – Ancilo can use them, and they reach your folder only if you keep them.

Until a folder is chosen there is nothing to type; an example clicked before asks for the folder first. Folders Ancilo worked in are listed in the Tasks area, each with its tasks; a click starts a new task there. Results you want somewhere else? Choose Documents (or any folder) as the task's folder.

## A folder: always in a copy

Ancilo never works in your folder directly. It works in a **copy** – one that holds only what the task changes, so a task starts at once, whatever the folder's size (Documents included). The task reads your folder as it is; before it changes a file, Ancilo notes the file's content (that is what the change starts from) and, where needed, puts it into the copy (on APFS a clone that takes no extra space). Hidden files, links and files that are only in iCloud are never part of it – and never touched.

A task's folder is a folder of your documents – Documents, or any folder in it or elsewhere. Your home folder itself, the folders above it, Library and the system's folders are refused: a task sees everything in its folder that is not hidden.

Files you give the task (drag them onto the input, or the paperclip) go into the copy – unless the folder already holds the same file (same name and content): then the task uses that one, without a second copy. The agent is told which files you gave.

The agent can:

| | |
|---|---|
| look | list the files, read documents (PDF by page, Word, Excel by sheet, CSV, text – and pictures and scans, by text recognition), find passages in all of them (up to 30 pictures per search) |
| write | new text files (`.txt`, `.md`, `.csv` and other plain text – nothing that runs when opened), Excel files (`write_spreadsheet`: numbers stay numbers, nothing ever becomes a formula), Word documents (`write_document`) |
| sort | move and rename files (a file keeps its ending) and folders (up to 2,000 files at once), make folders, delete files |

It cannot run commands or reach the network, and no path leads out of the copy (no `..`, no absolute paths, no links). Text in documents is treated as content, never as instructions.

There is nothing to set: the agent works on its own in the copy – your folder changes only when you keep the changes. You see its steps as it works and can stop it any time. Web searches follow the web search switch next to the input, as everywhere: off – each search shows its words and asks first; on – Ancilo searches by itself. Only the search words go out.

## Keep, drop, undo

When the agent is done, a card lists what changed: **new**, **changed**, **deleted**, **moved** (with where from).

### Look at it first

Every new or changed table, Word document and text gets checked before you decide – each shows *checked*, *n note(s)* or *n problem(s)*. **Look at it** shows the file as it is inside: a spreadsheet's sheets with their row numbers and column letters (as in Excel), a Word document's headings, paragraphs and tables. A cell a check found wrong is marked.

The checks follow fixed rules, by question. A *problem* is only what is sure; what Ancilo can only suppose is a *note* (marked "!") – it never blocks a file that is fine:

- **Readable** – the file opens and its contents can be read (a broken table or Word file is a problem; a Word file must be a complete package).
- **Complete** – the file is not empty, has more than a header row, and holds what you named: columns you asked for with "columns …" (in its header row, as whole words – an "Update" column is no "Date" column), sections as headings, words in quotes anywhere, and a total row if you asked for one under any name ("Summe", "Gesamtkosten", "Overall budget"). Saying no takes a request back ("ohne Datum", "Datum weg", "no total"); "Datum ist nicht optional" does not. When a task only describes a table ("a list with A, B and C"), its items count as columns only if the file has most of them as column heads; otherwise one not found is a note. A named column left empty in a row is a note too, unless the task says it may be.
- **Numbers** – totals add up (a subtotal over its group – also "Travel subtotal", "Zwischensumme Nord" – a grand total over all rows or the totals before it), and quantity × price (or hours × rate) = amount, row by row. A column is read one way throughout: 1,250 is 1.25 or 1250 in every row of it, never one here and the other there; a column that writes its numbers both ways is a note. A total stated in a document's text that does not match its table is a note (what the text means is not sure). Only the first 20,000 rows of a sheet are checked; if it has more, the check says so.

They cannot tell whether the content is right – only whether it holds together. Formulas are never calculated: a formula Ancilo was given is written as text (that is on purpose – nothing in a file runs), and the check says so; a workbook's own formulas show the value they had when it was saved. What the view leaves out is said below it: fonts, colours, pictures and charts; only the first 200 rows of a sheet (all of them are checked).

**What you saw is what you keep**: *Keep* (and *Save*) take exactly the version that was checked and shown. If the task changed its results since – say, after another message – nothing is written; Ancilo checks again and you look again. A view of an older version says so and offers the new one; it never changes under your eyes. When a check found problems, *Keep* asks first.

- **Keep** puts the changes into your folder. Before anything is written, Ancilo checks that the folder still holds what the copy started from – if you changed a file there meanwhile, or a file of a new name appeared, **nothing** is written and Ancilo says which files. Originals are backed up first; every step is noted in a journal; files are written next to their place and moved in at once. If a step fails, everything before it is put back. If Ancilo stops in the middle (a crash), it puts things back at the next start.
- **Drop** takes the changes out of the copy – the task sees your folder as it is again; your folder is not touched.
- **Undo** (after keeping) takes the changes back out of your folder – as long as nothing changed there since; folders that were made for the changes go too.

## From the command line or other tools

```bash
ancilo op create_task '{"folder": "/path/to/folder", "title": "Invoices"}'
ancilo op send_message '{"session": "s-…", "text": "Make a table of the invoices", "wait": true}'
ancilo op session_diff '{"session": "s-…"}'
ancilo op apply_changes '{"session": "s-…"}' --confirm
ancilo op undo_apply '{"session": "s-…"}' --confirm
```

`check_results` checks a task's results (with the `version` they were checked for), `preview_result` shows one of them; `apply_changes` and `save_results` take a `version` and refuse a newer one. `add_task_file` puts a file into a task's copy; `discard_changes` drops a task's changes. Without `folder`, `create_task` still starts a task without a folder of its own (the app no longer offers it; such tasks are listed under *Tasks* and saved with `save_results`, `dir` default Documents).
