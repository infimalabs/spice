const fs = require("fs");
const vm = require("vm");

const source = fs.readFileSync(process.argv[2], "utf8");

// Both media types a shipped speech backend can declare: audio/mp4 from the
// macOS say backend, audio/wav from the documented espeak-ng preset. Naming
// both is what proves the clip follows the response instead of trading one
// hardcoded constant for another.
const SERVED_CONTENT_TYPES = ["audio/wav", "audio/mp4"];
const CLIP_BYTE_LENGTH = 8;

function playbackContext(contentType, observedBlobTypes) {
  return {
    console,
    setTimeout,
    spiceServeBranding: { name: "spice" },
    document: { title: "", querySelectorAll: () => [] },
    navigator: { mediaSession: { setActionHandler() {} } },
    targetApi: (targetId, path) => `/api/work/${targetId}${path}`,
    fetch: async () => ({
      ok: true,
      headers: {
        get: (name) => (name === "Content-Type" ? contentType : null),
      },
      arrayBuffer: async () => new ArrayBuffer(CLIP_BYTE_LENGTH),
    }),
    Blob: class {
      constructor(parts, options) {
        observedBlobTypes.push(options.type);
      }
    },
    URL: {
      createObjectURL: () => "blob:speech-clip",
      revokeObjectURL() {},
    },
    Audio: class {
      constructor() {
        this.ended = true;
        this.handlers = {};
      }
      addEventListener(name, handler) {
        this.handlers[name] = handler;
      }
      removeEventListener(name) {
        delete this.handlers[name];
      }
      play() {
        // A real clip settles the playback promise from its ended event; the
        // stub ends immediately so the awaited playSpeech can return.
        setTimeout(() => this.handlers.ended(), 0);
        return Promise.resolve();
      }
      pause() {}
    },
  };
}

async function blobTypeForServedClip(contentType) {
  const observedBlobTypes = [];
  const context = playbackContext(contentType, observedBlobTypes);
  vm.createContext(context);
  vm.runInContext(source, context, { filename: "app.audio.js" });
  await context.playSpeech({ targetId: "target-fixture" }, "spice speech check");
  return observedBlobTypes;
}

async function main() {
  const observed = {};
  for (const contentType of SERVED_CONTENT_TYPES) {
    observed[contentType] = await blobTypeForServedClip(contentType);
  }
  const expected = {};
  for (const contentType of SERVED_CONTENT_TYPES) {
    expected[contentType] = [contentType];
  }
  const actualText = JSON.stringify(observed);
  const expectedText = JSON.stringify(expected);
  if (actualText !== expectedText) {
    throw new Error(
      `playback blob types should equal the served content types: ` +
        `expected ${expectedText}, observed ${actualText}`,
    );
  }
}

main().catch((error) => {
  console.error(error.message);
  process.exit(1);
});
