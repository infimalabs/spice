// Spice writes this oh-my-pi extension beside each seat's configuration
// overlay and loads it with `--extension`.
//
// omp wraps every steered user message in a generated notice telling the
// model it is a priority interjection that supersedes prior instructions.
// Spice's steering already says what it is, with its own authenticated key,
// so the provider sees the message as Spice wrote it. The `context` hook
// changes only the copies omp hands the provider: queue delivery, the stored
// transcript, attachments, and the steering flag on the persisted message are
// left as they are.
//
// Background notices replace one another by purpose while a step runs. The
// SDK aside queue delivers their single batch before the next model step.
export default function (pi) {
  // RPC exposes steer/follow-up but not aside. This command bridges that one
  // SDK operation. Replacements remain local until the next step boundary.
  const pending = new Map();
  // A per-purpose revision distinguishes an untouched empty slot from one a
  // replacement or clear deliberately left while an aside was in flight.
  const revisions = new Map();
  const inFlight = new Map();
  const flush = async () => {
    const entries = [...pending].map(([purpose, message]) => [
      purpose,
      message,
      revisions.get(purpose),
    ]);
    for (const [purpose] of entries) {
      inFlight.set(purpose, (inFlight.get(purpose) ?? 0) + 1);
    }
    pending.clear();
    if (!entries.length) return;
    try {
      await pi.sendUserMessage(
        entries.map(([, message]) => message).join("\n\n"),
        { deliverAs: "aside" },
      );
    } catch (error) {
      if (error?.name !== "PromptDroppedError") throw error;
      for (const [purpose, message, revision] of entries) {
        if (revisions.get(purpose) === revision && !pending.has(purpose)) {
          pending.set(purpose, message);
        }
      }
    } finally {
      for (const [purpose] of entries) {
        const active = inFlight.get(purpose) - 1;
        if (active) {
          inFlight.set(purpose, active);
        } else {
          inFlight.delete(purpose);
          if (!pending.has(purpose)) revisions.delete(purpose);
        }
      }
    }
  };
  pi.registerCommand("spice-aside", {
    description: "Deliver a Spice background notice at the next step boundary",
    handler: async (args, ctx) => {
      const { purpose, message } = JSON.parse(args);
      if (typeof purpose !== "string" || !purpose || typeof message !== "string") {
        throw new Error("spice-aside requires a purpose and a message");
      }
      revisions.set(purpose, (revisions.get(purpose) ?? 0) + 1);
      if (message) {
        pending.set(purpose, message);
      } else {
        pending.delete(purpose);
        if (!inFlight.has(purpose)) revisions.delete(purpose);
      }
      if (ctx.isIdle()) await flush();
    },
  });
  pi.on("turn_end", flush);
  pi.on("agent_end", flush);
  pi.on("context", (event) => ({
    messages: event.messages.map((message) =>
      message.role === "user" && message.steering === true
        ? { ...message, steering: false }
        : message,
    ),
  }));
}
