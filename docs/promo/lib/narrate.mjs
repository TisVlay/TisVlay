// Turns the narration text into speech: one clip per sentence, with
// the time each word is spoken, for the captions and the shots' timing.
//
// The text is docs/promo/narration/<tone>/<shot>.md, a sentence per
// line. The speech comes from a Kokoro-FastAPI server, whose captioned
// endpoint returns the audio and its word timings in one call. Clips
// are cached by text and voice, so only changed sentences are spoken
// again, and a render needs no server once they exist.
//
//     node lib/narrate.mjs <server url> <voice>
import { createHash } from "node:crypto";
import { existsSync, mkdirSync, readdirSync, readFileSync, writeFileSync } from "node:fs";
import { basename, join } from "node:path";

const [server, voice] = process.argv.slice(2);
if (!server || !voice) throw new Error("usage: narrate.mjs <server url> <voice>");

const source = "narration";
const cache = "build/narration";
mkdirSync(cache, { recursive: true });

async function speak(text) {
  const key = createHash("sha256").update(`${voice}\n${text}`).digest("hex").slice(0, 16);
  const audio = join(cache, `${key}.mp3`);
  const timings = join(cache, `${key}.json`);
  if (!existsSync(audio) || !existsSync(timings)) {
    const reply = await fetch(`${server}/dev/captioned_speech`, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({
        model: "kokoro",
        input: text,
        voice,
        response_format: "mp3",
        stream: false,
        return_timestamps: true,
      }),
    });
    if (!reply.ok) throw new Error(`${server} answered ${reply.status} for: ${text}`);
    const body = await reply.json();
    writeFileSync(audio, Buffer.from(body.audio, "base64"));
    writeFileSync(timings, JSON.stringify(body.timestamps));
  }
  const words = JSON.parse(readFileSync(timings, "utf8"));
  return { text, audio, duration: words.at(-1).end_time, words };
}

const narration = {};
for (const tone of readdirSync(source).sort()) {
  narration[tone] = {};
  for (const file of readdirSync(join(source, tone)).sort()) {
    const lines = readFileSync(join(source, tone, file), "utf8")
      .split("\n")
      .map((line) => line.trim())
      .filter(Boolean);
    narration[tone][basename(file, ".md")] = [];
    for (const line of lines) narration[tone][basename(file, ".md")].push(await speak(line));
  }
}

// The form a composition can load: a script, not a file read at render.
writeFileSync(join(cache, "narration.js"), `window.__promoNarration = ${JSON.stringify(narration)};\n`);
const count = Object.values(narration).flatMap((shots) => Object.values(shots)).flat().length;
console.log(`${count} sentences in ${Object.keys(narration).length} tones`);
