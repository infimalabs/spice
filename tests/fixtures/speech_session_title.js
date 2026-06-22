const fs = require("fs");
const vm = require("vm");

const source = fs.readFileSync(process.argv[2], "utf8");
const context = {
  console,
  document: { title: "" },
  MediaMetadata: class {
    constructor(values) {
      Object.assign(this, values);
    }
  },
  navigator: {
    mediaSession: {
      metadata: null,
      playbackState: "",
      setActionHandler() {},
    },
  },
  spiceServeBranding: { name: "spice" },
};

vm.createContext(context);
vm.runInContext(source, context, { filename: "app.audio.js" });

function assert(condition, message) {
  if (!condition) throw new Error(message);
}

assert(
  context.speechSessionTitle({ targetLane: { agentName: "main-b" } }) ===
    "main-b - spice",
  "agent name should lead the speech session title",
);
assert(
  context.speechSessionTitle({ targetLane: { branchName: "spice-b" } }) ===
    "spice-b - spice",
  "branch fallback should lead the speech session title",
);
assert(
  context.speechSessionTitle({
    lane: { agentName: "source" },
    targetLane: { branchName: "target" },
  }) === "target - spice",
  "target lane identity should win for fused-lane playback",
);
assert(
  context.speechSessionTitle({ targetLane: { agentName: "spice" } }) ===
    "spice",
  "duplicate brand-only identity should collapse",
);
assert(
  context.speechSessionTitle({ targetLane: {} }) === "spice",
  "missing lane identity should keep the default title",
);

vm.runInContext(
  'currentSpeech = { targetLane: { agentName: "main-b" }, messageKey: "m" }; syncSpeechSessionMetadata();',
  context,
);
assert(
  context.document.title === "main-b - spice",
  "document title should lead with the agent name during playback",
);
assert(
  context.navigator.mediaSession.metadata.title === "main-b - spice",
  "media metadata title should lead with the agent name during playback",
);
assert(
  context.navigator.mediaSession.metadata.artist === "spice",
  "media metadata artist should retain the serve brand",
);
