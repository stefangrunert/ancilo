# Where Ancilo's code comes from

Most of Ancilo is written for Ancilo. A few parts were **ported** from other open-source projects – translated into Ancilo's language and architecture, measured against the original, and kept only where they made Ancilo better. Their license texts ship with the app (*Licenses* in `Ancilo.app/Contents/Resources/licenses`, `THIRD_PARTY_NOTICES.txt`) and are in [`third_party/ported`](../third_party/ported).

This page lists what was taken over, what was changed, what was measured and not taken.

## Long tool results – Atomic Agent

| | |
|---|---|
| Source | [AtomicBot-ai/atomic-agent](https://github.com/AtomicBot-ai/atomic-agent) at `02aa8a9aec6408e6e04a325932d970e70ea45174`: `src/compressor/result-compressor.ts`, `listing-caps.ts`, `log-summarizer.ts` and their tests |
| License | MIT, © 2026 Atomic Bot |
| In Ancilo | [`crates/agent/src/compress.rs`](../crates/agent/src/compress.rs) – the port, with all the original tests translated |
| Checked | The port and the original TypeScript give identical results on 96 runs (two case sets × 4 option sets); they differ only where JavaScript counts UTF-16 units (an emoji is 1 character here, 2 there) – `evals/fpl02/run-original.mjs`, `crates/agent/tests/fpl02.rs` |
| Differences | lengths in characters, never splitting one; a budget smaller than the marker leaves an empty cut (the original's `slice(0, negative)` kept almost everything); whether a result failed comes from the tool, not from the words in it |

Ancilo's own part ([`crates/agent/src/results.rs`](../crates/agent/src/results.rs)) decides **which** cut fits **which** output: a command's output keeps its error lines (with the line after each – where *expected/got* usually is), its end and, in a log of repeated lines, the lines that do not fit the pattern; a listing keeps its first rows and the tool's closing note; a text its start (and, if repetitive, its odd lines). Every result keeps its tool and status, and long results stay whole with the conversation for `read_result`.

On its own the port did **worse** than Ancilo before on lists and documents (it always keeps the end): 2 of 12 development cases against 3 of 12. With the cut by kind: 12 of 12, and on an independent set (written by a second agent, unseen until measured) 2 of 12 on first sight – 12 of 12 after revision, which is not counted as independent evidence.

## Document passages – AnythingLLM / LangChain (measured, not taken)

| | |
|---|---|
| Source | [Mintplex-Labs/anything-llm](https://github.com/Mintplex-Labs/anything-llm) at `ead123f07befc7167d05f160dade56aed5fa7c10`: `server/utils/TextSplitter/index.js`, which wraps `@langchain/textsplitters` 0.0.0 (`RecursiveCharacterTextSplitter`, npm integrity `sha512-3hPesWom…Saw==`, as pinned in AnythingLLM's `yarn.lock`) |
| License | MIT, © Mintplex Labs Inc.; MIT, © 2023 LangChain |
| In Ancilo | [`crates/docs/src/split.rs`](../crates/docs/src/split.rs) – the splitter with its line counting; identical to LangChain's output on 28 runs (`evals/fpl01/run-langchain.mjs`) |
| Used? | **No.** Measured against Ancilo's own passages (`crates/docs/tests/fpl01.rs`) on a development set and two independent sets: at the same passage size it chose no better; AnythingLLM's document name on every passage helped where a question named a sheet or folder and hurt where several files looked alike. A variant that weighs the names separately was refuted by an independent set. Ancilo keeps choosing passages as before; the port stays for measuring later versions. |

What Ancilo built for sources instead – marks Ancilo gives and checks, the passage as the answer had it, the document's version – is its own ([`crates/docs/src/evidence.rs`](../crates/docs/src/evidence.rs)).

## Looking at results – Jan

| | |
|---|---|
| Source | [janhq/jan](https://github.com/janhq/jan) at `b7f4f641efdcacaeca7201506c52900e7539f374`: `web-app/src/lib/coworkPreview.ts` (preview states, file kinds) and its tests |
| License | Apache-2.0, © 2025 Menlo Research – *This product includes software developed by Menlo Research (https://menlo.ai).* |
| In Ancilo | [`app/src/state/preview.ts`](../app/src/state/preview.ts) and its tests: the states a preview goes through (loading, ready, not shown, failed), *changed since – offered, never swapped*, the kind of a file by its name |
| Changed | the kinds are what Ancilo shows of a task's results; the file is read by the daemon in its sandboxed reader, never by the web view; a ready preview carries the version it shows |

Neither Jan nor its fork Atomic Chat shows Word or Excel files or checks them. The view of tables and documents and the checks (readable, complete, numbers) are Ancilo's own ([`crates/docs/src/preview.rs`](../crates/docs/src/preview.rs)).
