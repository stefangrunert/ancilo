// Runs Atomic Agent's original compressor (TypeScript, from the source
// checkout) on the FPL-02 cases with several options – the reference the
// Rust port is compared with (crates/agent/tests/fpl02.rs).
//   node --experimental-strip-types evals/fpl02/run-original.mjs <atomic-agent dir> [cases.json]
// (FPL02_ORIGINAL=<file>: where the results go; default original.json)
import { readFileSync, writeFileSync } from "node:fs";
import { join, dirname } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const src = process.argv[2];
const files = process.argv.slice(3);
if (!files.length) files.push(join(here, "dev-cases.json"));
const { compressToolResult } = await import(join(src, "src/compressor/result-compressor.ts"));
const { listingResultCaps } = await import(join(src, "src/compressor/listing-caps.ts"));

const runs = [];
for (const f of files) {
  for (const c of JSON.parse(readFileSync(f, "utf8"))) {
    const status = c.is_error ? "error" : "ok";
    const astral = /[\u{10000}-\u{10FFFF}]/u.test(c.output);
    const sets = {
      defaults: {},
      budget_head: { maxSummaryLength: c.budget_chars, maxTailLines: 12 },
      budget_tail: { maxSummaryLength: c.budget_chars, maxTailLines: 40, overflow: "tail" },
      listing: listingResultCaps(50, 120),
    };
    for (const [name, options] of Object.entries(sets)) {
      const out = compressToolResult({ tool: c.tool, status, output: c.output }, options);
      const o = { maxSummaryLength: 400, maxTailLines: 12, overflow: "head", ...options };
      if (o.maxTailLines === Number.MAX_SAFE_INTEGER) o.maxTailLines = null;
      runs.push({ id: `${c.id}/${name}`, case: c.id, status, astral, options: o, summary: out.summary, truncated: out.truncated });
    }
  }
}
const out = process.env.FPL02_ORIGINAL ?? join(here, "original.json");
writeFileSync(out, JSON.stringify(runs, null, 0));
console.log(`${runs.length} runs`);
