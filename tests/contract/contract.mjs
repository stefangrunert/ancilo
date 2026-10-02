// Contract test: the official OpenAI and Anthropic SDKs as clients of
// Ancilo's model API. Driven by crates/daemon/tests/contract.rs, which starts
// a daemon with a scripted fake model; the script's steps match the order of
// the calls below.
//
// Usage: node contract.mjs <base-url> <token>

import OpenAI from "openai";
import Anthropic from "@anthropic-ai/sdk";

const [baseURL, token] = process.argv.slice(2);
let failures = 0;
const check = (cond, what, detail = "") => {
  if (cond) {
    console.log(`ok   ${what}`);
  } else {
    failures += 1;
    console.log(`FAIL ${what} ${detail}`);
  }
};

const tools = [
  {
    type: "function",
    function: {
      name: "read_file",
      description: "Read a file",
      parameters: { type: "object", properties: { path: { type: "string" } }, required: ["path"] },
    },
  },
];

// ---- OpenAI SDK (M2-AC-01) -------------------------------------------------
const openai = new OpenAI({ baseURL: `${baseURL}/v1`, apiKey: token, maxRetries: 0 });

{
  const r = await openai.chat.completions.create({ model: "default", messages: [{ role: "user", content: "Say hello" }] });
  check(r.choices[0].message.content === "Hello from Ancilo", "openai: text", JSON.stringify(r));
  check(typeof r.usage?.completion_tokens === "number", "openai: usage");
}
{
  const stream = await openai.chat.completions.create({
    model: "default",
    stream: true,
    messages: [{ role: "user", content: "Stream please" }],
  });
  let text = "";
  for await (const chunk of stream) text += chunk.choices[0]?.delta?.content ?? "";
  check(text === "streamed answer here", "openai: streaming", JSON.stringify(text));
}
{
  const r = await openai.chat.completions.create({
    model: "default",
    tools,
    messages: [{ role: "user", content: "Read main.rs" }],
  });
  const call = r.choices[0].message.tool_calls?.[0];
  check(call?.function?.name === "read_file", "openai: tool call name", JSON.stringify(r));
  check(JSON.parse(call?.function?.arguments ?? "{}").path === "main.rs", "openai: tool call arguments");
  check(r.choices[0].finish_reason === "tool_calls", "openai: finish_reason tool_calls");
}
{
  const stream = await openai.chat.completions.create({
    model: "default",
    stream: true,
    tools,
    messages: [{ role: "user", content: "Read lib.rs, streamed" }],
  });
  let name = "";
  let args = "";
  for await (const chunk of stream) {
    for (const tc of chunk.choices[0]?.delta?.tool_calls ?? []) {
      name += tc.function?.name ?? "";
      args += tc.function?.arguments ?? "";
    }
  }
  check(name === "read_file" && JSON.parse(args).path === "lib.rs", "openai: streamed tool call", `${name} ${args}`);
}
try {
  await openai.chat.completions.create({ model: "default", messages: [{ role: "user", content: "fail please" }] });
  check(false, "openai: error is raised");
} catch (e) {
  check(e instanceof OpenAI.APIError && e.status === 503, "openai: HTTP error surfaces as APIError", `${e}`);
}
{
  const list = await openai.models.list();
  const ids = list.data.map((m) => m.id);
  check(ids.includes("default"), "openai: models list includes the default role", JSON.stringify(ids));
}

// ---- Anthropic SDK (M2-AC-02) ----------------------------------------------
const anthropic = new Anthropic({ baseURL, apiKey: token, maxRetries: 0 });
const atools = [
  { name: "read_file", description: "Read a file", input_schema: { type: "object", properties: { path: { type: "string" } }, required: ["path"] } },
];

{
  const m = await anthropic.messages.create({
    model: "claude-sonnet-4-6",
    max_tokens: 256,
    system: "You are terse.",
    messages: [{ role: "user", content: "Greet me" }],
  });
  check(m.type === "message" && m.content[0]?.type === "text" && m.content[0].text === "Hi from the Anthropic path", "anthropic: text", JSON.stringify(m));
  check(m.stop_reason === "end_turn", "anthropic: stop_reason end_turn");
  check(typeof m.usage.output_tokens === "number", "anthropic: usage");
}
let toolUse;
{
  const m = await anthropic.messages.create({
    model: "claude-sonnet-4-6",
    max_tokens: 256,
    tools: atools,
    messages: [{ role: "user", content: "Read main.rs" }],
  });
  toolUse = m.content.find((b) => b.type === "tool_use");
  check(toolUse?.name === "read_file" && toolUse?.input?.path === "main.rs", "anthropic: tool_use block", JSON.stringify(m));
  check(m.stop_reason === "tool_use", "anthropic: stop_reason tool_use");
}
{
  const m = await anthropic.messages.create({
    model: "claude-sonnet-4-6",
    max_tokens: 256,
    tools: atools,
    messages: [
      { role: "user", content: "Read main.rs" },
      { role: "assistant", content: [toolUse] },
      { role: "user", content: [{ type: "tool_result", tool_use_id: toolUse.id, content: "fn main() {}" }] },
    ],
  });
  check(m.content[0]?.text === "It has one function.", "anthropic: tool_result round trip", JSON.stringify(m));
}
{
  const stream = anthropic.messages.stream({
    model: "claude-sonnet-4-6",
    max_tokens: 256,
    messages: [{ role: "user", content: "Stream please" }],
  });
  let deltas = "";
  stream.on("text", (t) => (deltas += t));
  const final = await stream.finalMessage();
  check(final.content[0]?.text === "streaming works", "anthropic: streaming final message", JSON.stringify(final));
  check(deltas === "streaming works", "anthropic: streaming text events", JSON.stringify(deltas));
}
{
  const stream = anthropic.messages.stream({
    model: "claude-sonnet-4-6",
    max_tokens: 256,
    tools: atools,
    messages: [{ role: "user", content: "Read lib.rs, streamed" }],
  });
  const final = await stream.finalMessage();
  const tu = final.content.find((b) => b.type === "tool_use");
  check(tu?.name === "read_file" && tu?.input?.path === "lib.rs", "anthropic: streamed tool_use", JSON.stringify(final));
  check(final.stop_reason === "tool_use", "anthropic: streamed stop_reason");
}
{
  const r = await anthropic.messages.countTokens({ model: "claude-sonnet-4-6", messages: [{ role: "user", content: "count these tokens please" }] });
  check(r.input_tokens > 0, "anthropic: count_tokens", JSON.stringify(r));
}
try {
  await anthropic.messages.create({ model: "x", max_tokens: 10, messages: [{ role: "user", content: "fail please" }] });
  check(false, "anthropic: error is raised");
} catch (e) {
  check(e instanceof Anthropic.APIError && e.status >= 500, "anthropic: HTTP error surfaces as APIError", `${e}`);
}

console.log(failures === 0 ? "ALL CONTRACT CHECKS PASSED" : `${failures} CONTRACT CHECK(S) FAILED`);
process.exit(failures === 0 ? 0 : 1);
