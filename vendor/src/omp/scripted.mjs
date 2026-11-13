// Spice writes this oh-my-pi extension for a scripted seat, with the lane's
// parsed cue plan embedded, and loads it with `--extension`.
//
// It registers the provider behind the scripted model. omp keeps its own agent
// loop, tools, compaction, and transcript; each model request takes the next
// reply cue, no earlier than the cue's scaled time, and nothing reaches a model
// service. A compaction cue starts when the reply before it ends its turn; that
// reply is held until the compaction's own time, because a settled occupant
// may be retired before a later compaction could start. The cue's summary
// answers omp's compaction hook, so compaction itself asks no model request.
import { AssistantMessageEventStream } from "@oh-my-pi/pi-ai";

const plan = __SPICE_SCRIPTED_PLAN__;
const started = Date.now();
let next = 0;
// The summary of the scripted compaction in progress, if any.
let summary;

// Cue times are offsets from the occupant's start on the scaled clock.
function due(cue) {
  const delay = started + cue.due_ms - Date.now();
  return delay > 0 ? new Promise((resolve) => setTimeout(resolve, delay)) : Promise.resolve();
}

// omp's compaction keeps a recent-token budget; this floor lets a short
// scripted context still compact.
const INPUT_TOKEN_FLOOR = 10500;

function usage(context) {
  const input = Math.ceil(JSON.stringify(context.messages || []).length / 4) + INPUT_TOKEN_FLOOR;
  return { input, output: 0, cacheRead: 0, cacheWrite: 0, totalTokens: input,
    reasoningTokens: 0, cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0, total: 0 },
    premiumRequests: 0 };
}

function message(model, content, stopReason, context) {
  return { role: "assistant", content, api: "spice-scripted", provider: "spice-scripted",
    model: model.id, usage: usage(context), stopReason, timestamp: Date.now() };
}

// The first delivered steering key the context has not yet answered.
function pendingKey(context) {
  const text = JSON.stringify(context.messages || []);
  const answered = new Set([...text.matchAll(/(?:ACK|NAK) ([A-Za-z0-9]+):/g)].map((match) => match[1]));
  const delivered = [
    ...text.matchAll(/directive=[^ ]+ key=([A-Za-z0-9]+)/g),
    ...text.matchAll(/Pending steering[\s\S]*?- ([A-Za-z0-9]+) \(/g),
  ].map((match) => match[1]);
  return delivered.find((key) => !answered.has(key));
}

function reply(model, context, cue) {
  if (cue.kind === "say") return { text: cue.text };
  if (cue.kind === "call") return { name: cue.name, arguments: cue.arguments };
  const key = pendingKey(context);
  if (!key) throw new Error(`line ${cue.line}: no delivered steering directive awaits an answer`);
  const verb = cue.kind === "decline" ? "NAK" : "ACK";
  return { name: "write", arguments: { i: "Answering scripted steering", path: "spice://outbox",
    content: `${verb} ${key}: ${cue.text}` } };
}

async function play(stream, model, context) {
  const cue = plan.cues[next];
  if (!cue) throw new Error("omp asked for a reply after the cue script ended");
  if (cue.kind === "compact") {
    throw new Error(`line ${cue.line}: omp asked for a reply where the script holds a compaction`);
  }
  next += 1;
  await due(cue);
  const following = plan.cues[next];
  if (following?.kind === "compact") await due(following);
  const answer = reply(model, context, cue);
  if (answer.text !== undefined) {
    const done = message(model, [{ type: "text", text: answer.text }], "stop", context);
    stream.push({ type: "start", partial: message(model, [], "stop", context) });
    stream.push({ type: "text_start", contentIndex: 0, partial: done });
    stream.push({ type: "text_delta", contentIndex: 0, delta: answer.text, partial: done });
    stream.push({ type: "text_end", contentIndex: 0, content: answer.text, partial: done });
    stream.push({ type: "done", reason: "stop", message: done });
    return;
  }
  const id = `scripted-${cue.line}`;
  const call = { type: "toolCall", id, name: answer.name, arguments: answer.arguments };
  const done = message(model, [call], "toolUse", context);
  stream.push({ type: "start", partial: message(model, [], "toolUse", context) });
  stream.push({ type: "toolcall_start", contentIndex: 0, id, name: answer.name, partial: done });
  stream.push({ type: "toolcall_delta", contentIndex: 0, delta: JSON.stringify(answer.arguments), partial: done });
  stream.push({ type: "toolcall_end", contentIndex: 0, toolCall: call, partial: done });
  stream.push({ type: "done", reason: "toolUse", message: done });
}

function streamSimple(model, context) {
  const stream = new AssistantMessageEventStream();
  play(stream, model, context).catch((error) => stream.fail(new Error(`scripted cue: ${error.message}`)));
  return stream;
}

export default function (pi) {
  pi.registerProvider("spice-scripted", {
    baseUrl: "http://127.0.0.1.invalid",
    apiKey: "unused",
    api: "spice-scripted",
    models: [{ id: "scripted", name: "Spice scripted occupant", reasoning: false,
      input: ["text"], contextWindow: 200000, maxTokens: 8192,
      cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0 } }],
    streamSimple,
  });
  pi.on("session_before_compact", (event) => {
    if (summary === undefined) return undefined;
    const { firstKeptEntryId, tokensBefore } = event.preparation;
    return { compaction: { summary, firstKeptEntryId, tokensBefore } };
  });
  pi.on("turn_end", async (_event, ctx) => {
    const cue = plan.cues[next];
    if (summary !== undefined || !cue || cue.kind !== "compact") return;
    summary = cue.summary;
    next += 1;
    try {
      await ctx.compact({ mode: "soft", suppressContinuation: false });
    } finally {
      summary = undefined;
    }
  });
}
