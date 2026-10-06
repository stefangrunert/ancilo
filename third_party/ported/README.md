# Ported source code

Code that Ancilo translated from other projects into Rust. The license texts
here go into the app's third-party notices (`xtask notices`).

| Source | Revision | License | Ported to | Used by Ancilo |
|---|---|---|---|---|
| [Atomic Agent](https://github.com/AtomicBot-ai/atomic-agent) `src/compressor/result-compressor.ts`, `listing-caps.ts`, `log-summarizer.ts` and their tests | `02aa8a9aec6408e6e04a325932d970e70ea45174` | MIT, © 2026 Atomic Bot | `crates/agent/src/compress.rs` | yes – long tool results (`crates/agent/src/results.rs`) |
| [Jan](https://github.com/janhq/jan) `web-app/src/lib/coworkPreview.ts` (preview states, file kinds) and its tests | `b7f4f641efdcacaeca7201506c52900e7539f374` | Apache-2.0, © 2025 Menlo Research – "This product includes software developed by Menlo Research (https://menlo.ai)." | `app/src/state/preview.ts` | yes – looking at a task's results before keeping them |
| [LangChain textsplitters](https://www.npmjs.com/package/@langchain/textsplitters) 0.0.0 `RecursiveCharacterTextSplitter`, as used by [AnythingLLM](https://github.com/Mintplex-Labs/anything-llm) `server/utils/TextSplitter` | npm `sha512-3hPesWom…Saw==`; AnythingLLM `ead123f07befc7167d05f160dade56aed5fa7c10` | MIT, © 2023 LangChain; MIT, © Mintplex Labs Inc. | `crates/docs/src/split.rs` | measured only (FPL-01): not better than Ancilo's passages, so not used to choose them |

Details of each port, its differences from the original and the measurements:
`docs/provenance.md`.
