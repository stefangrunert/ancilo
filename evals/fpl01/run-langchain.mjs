// Runs LangChain's RecursiveCharacterTextSplitter (the version AnythingLLM
// pins, from the npm package) on sample texts – the reference the Rust port
// in crates/docs/src/split.rs is compared with.
//   node evals/fpl01/run-langchain.mjs <dir of @langchain/textsplitters 0.0.0> [texts.json]
import { readFileSync, writeFileSync } from "node:fs";
import { join, dirname } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const pkg = process.argv[2];
// The package imports @langchain/core only for Document/tiktoken; the
// splitting itself does not need it – a minimal stand-in is provided.
const src = readFileSync(join(pkg, "package/dist/text_splitter.js"), "utf8")
  .replace(/^import .*@langchain\/core\/documents.*$/m, "class Document { constructor(f) { Object.assign(this, f); } } class BaseDocumentTransformer { constructor() {} }")
  .replace(/^import .*@langchain\/core\/utils\/tiktoken.*$/m, "const getEncoding = () => { throw new Error('no tiktoken'); };");
const tmp = join(here, ".text_splitter.tmp.mjs");
writeFileSync(tmp, src);
const { RecursiveCharacterTextSplitter } = await import(pathToFileURL(tmp).href);

const texts = process.argv[3] ? JSON.parse(readFileSync(process.argv[3], "utf8")) : samples();
const runs = [];
for (const t of texts) {
  for (const [chunkSize, chunkOverlap] of [[1000, 20], [400, 50], [120, 10], [30, 0]]) {
    const s = new RecursiveCharacterTextSplitter({ chunkSize, chunkOverlap });
    const docs = await s.createDocuments([t.text]);
    runs.push({
      id: `${t.id}/${chunkSize}-${chunkOverlap}`, chunkSize, chunkOverlap, text: t.text,
      astral: /[\u{10000}-\u{10FFFF}]/u.test(t.text),
      chunks: docs.map((d) => ({ text: d.pageContent, from: d.metadata.loc.lines.from, to: d.metadata.loc.lines.to })),
    });
  }
}
writeFileSync(join(here, "langchain.json"), JSON.stringify(runs));
console.log(`${runs.length} runs`);

function samples() {
  const para = (i) => `Absatz ${i}: Der Mieter zahlt die Nebenkosten monatlich im Voraus. Die Abrechnung erfolgt jährlich bis zum 30. Juni.`;
  return [
    { id: "short", text: "Kurzer Text." },
    { id: "paragraphs", text: Array.from({ length: 40 }, (_, i) => para(i)).join("\n\n") },
    { id: "lines", text: Array.from({ length: 200 }, (_, i) => `Zeile ${i} | Konto ${1000 + i} | Betrag ${(i * 17) % 500},00 EUR`).join("\n") },
    { id: "triple-newlines", text: "Kopf\n\n\nText nach drei Umbrüchen.\n\n\n\nNoch einer.\n" + "Wort ".repeat(300) },
    { id: "one-long-word", text: "x".repeat(2500) + " Ende" },
    { id: "umlauts", text: Array.from({ length: 60 }, (_, i) => `Größe ${i}: Übergabe der Schlüssel – Frist äußerst knapp.`).join("\n") },
    { id: "mixed", text: "§ 1 Gegenstand\n\nDer Vertrag regelt die Lieferung.\n§ 2 Laufzeit\nBeginn 1.1.2027, Ende 31.12.2028.\n\n\n§ 3 Kündigung\n" + "Die Kündigung bedarf der Schriftform. ".repeat(40) },
  ];
}
