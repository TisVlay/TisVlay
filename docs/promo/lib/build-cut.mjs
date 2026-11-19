// Lays a cut out in time and writes its composition.
//
// A cut (docs/promo/cuts/<cut>.json) is a list of shots. Each shot runs
// as long as its narration in the chosen tone, or as long as its
// footage if that is longer, and cross-fades into the next. The
// renderer finds media by reading the page, so the audio and video
// elements are written here, with their times, not built by a script.
//
//     node lib/build-cut.mjs <cut> <tone>
import { execFileSync } from "node:child_process";
import { copyFileSync, existsSync, mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { basename } from "node:path";

const [cutName, tone] = process.argv.slice(2);
if (!cutName || !tone) throw new Error("usage: build-cut.mjs <cut> <tone>");

// Seconds: before a shot's first sentence, between sentences, after the
// last, and the cross-fade between shots.
const LEAD = 0.6;
const GAP = 0.45;
const TAIL = 0.8;
const FADE = 0.35;
// A beat's action follows its sentence's first word by this much, and a
// shot holds this long after the stack's last reaction.
const CUE = 0.3;
const SETTLE = 1.8;

const W = 3840;
const H = 2160;
// Where a framed window's content sits: the settings window's clip at
// 0.8 of its pixels, under a title bar.
const FRAME = { x: 768, y: 208, w: 2304, h: 1536, bar: 88 };
// The TV a full-screen clip plays on.
const TV = { x: 560, y: 200, w: 2560, h: 1496, inset: 28 };

const cut = JSON.parse(readFileSync(`cuts/${cutName}.json`, "utf8"));
const narration = JSON.parse(
  readFileSync("build/narration/narration.js", "utf8").replace(/^window\.__promoNarration = /, "").replace(/;\s*$/, ""),
)[tone];
if (!narration) throw new Error(`no narration in the ${tone} tone`);
const version = /^version = "([^"]+)"/m.exec(readFileSync("../../Cargo.toml", "utf8").split("[workspace.package]")[1])[1];

const timeline = (name) =>
  readFileSync(`build/timelines/${name}.jsonl`, "utf8").trim().split("\n").map((line) => JSON.parse(line));

// A clip's length, where it starts on its shot's timeline, and its last
// frame, which holds once the clip has played out.
function clip(name) {
  const entries = timeline(name);
  const entry = entries.find((e) => e.kind === "clip");
  const last = `build/clips/${name}.last.png`;
  execFileSync("ffmpeg", ["-v", "error", "-y", "-sseof", "-0.2", "-i", `build/clips/${name}.mp4`, "-update", "1", "-frames:v", "1", last]);
  return { name, duration: entry.duration, began: entry.t - entry.duration, entries };
}

// The actions the stage shows the PC taking; lib/stage.js draws the same set.
const isBeat = (e) =>
  e.kind === "action" &&
  (["lock", "unlock", "sleep", "wake", "return"].includes(e.do) || (e.do === "cli" && e.args[0] === "stream"));

let at = 0;
const shots = [];
const media = [];
const scripts = new Set();
cut.shots.forEach((shot, index) => {
  const lines = narration[shot.shot] ?? [];
  let t = LEAD;
  const sentences = lines.map((line) => {
    const sentence = { text: line.text, audio: line.audio, start: t, duration: line.duration };
    t += line.duration + GAP;
    return sentence;
  });
  let duration = Math.max(3, t - GAP + TAIL);
  const laid = { ...shot, index, sentences };
  // Images are written into the page too: the renderer serves what the
  // page names, not what a script asks for later.
  const image = (src) => {
    const id = `image-${index}-${media.length}`;
    media.push(`<img id="${id}" class="asset" src="${src}" />`);
    return id;
  };
  // An image from elsewhere in the repository, copied in: the renderer
  // serves nothing outside the project folder.
  const borrowed = (path) => {
    mkdirSync("build/assets", { recursive: true });
    copyFileSync(`../../${path}`, `build/assets/${basename(path)}`);
    return image(`../assets/${basename(path)}`);
  };
  if (shot.kind === "title") laid.banner = borrowed("docs/artwork/banner.png");
  if (shot.kind === "stills") laid.images = shot.images.map(borrowed);

  if (shot.kind === "stage") {
    // Each beat's action lands on its sentence; actions sharing a
    // sentence keep the spacing they were recorded with.
    const entries = timeline(shot.timeline);
    const origin = entries.find((e) => e.kind === "action").t;
    const acts = entries.filter(isBeat);
    laid.beatTimes = [];
    acts.forEach((act, i) => {
      const sentence = sentences[shot.beats[i]];
      const first = i === 0 || shot.beats[i] !== shot.beats[i - 1];
      laid.beatTimes.push(first && sentence ? sentence.start + CUE : laid.beatTimes[i - 1] + (act.t - acts[i - 1].t));
    });
    const lastAct = acts.at(-1);
    const lastState = entries.filter((e) => e.kind === "state" && e.key === "state/power").at(-1);
    duration = Math.max(duration, laid.beatTimes.at(-1) + Math.max(0, lastState.t - lastAct.t) + SETTLE);
    laid.origin = origin;
    scripts.add(`../timelines/${shot.timeline}.js`);
  }
  if (shot.kind === "window" || shot.kind === "tv") {
    const footage = clip(shot.clip);
    duration = Math.max(duration, Math.min(footage.duration, duration + 4));
    const box = shot.kind === "window" ? FRAME : { x: TV.x + TV.inset, y: TV.y + TV.inset, w: TV.w - 2 * TV.inset, h: TV.h - 2 * TV.inset };
    laid.box = box;
    laid.last = image(`../clips/${shot.clip}.last.png`);
    // Key presses, in the shot's time, for the remote beside the TV.
    laid.keys = footage.entries.filter((e) => e.kind === "action" && e.do === "key").map((e) => e.t - footage.began);
    media.push(
      `<video id="clip-${index}" class="clip footage" src="../clips/${shot.clip}.mp4" muted playsinline data-start="${at.toFixed(3)}" data-duration="${Math.min(footage.duration, duration).toFixed(3)}" style="left:${box.x}px;top:${box.y}px;width:${box.w}px;height:${box.h}px"></video>`,
    );
  }
  laid.start = at;
  laid.duration = duration;
  sentences.forEach((sentence, i) => {
    media.push(
      `<audio id="say-${index}-${i}" src="../../${sentence.audio}" data-start="${(at + sentence.start).toFixed(3)}" data-duration="${sentence.duration.toFixed(3)}"></audio>`,
    );
  });
  shots.push(laid);
  at += duration - FADE;
});
const total = at + FADE;

for (const script of scripts) if (!existsSync(`build/cuts/${script}`.replace("build/cuts/../", "build/"))) throw new Error(`missing ${script}`);

const page = `<!doctype html>
<html lang="en">
  <head>
    <meta charset="utf-8" />
    <title>${cutName}, ${tone}</title>
    <link rel="stylesheet" href="../../lib/stage.css" />
  </head>
  <body>
    <!-- Generated by lib/build-cut.mjs from cuts/${cutName}.json in the ${tone} tone. -->
    <div id="cut" class="stage" data-composition-id="cut" data-width="${W}" data-height="${H}" data-fps="60" data-start="0" data-duration="${total.toFixed(3)}">
      ${media.join("\n      ")}
    </div>
    <script src="../../node_modules/gsap/dist/gsap.min.js"></script>
    ${[...scripts].map((src) => `<script src="${src}"></script>`).join("\n    ")}
    <script src="../../lib/stage.js"></script>
    <script src="../../lib/cut.js"></script>
    <script>
      window.__timelines = window.__timelines || {};
      window.__timelines["cut"] = Promo.cut(document.querySelector("#cut"), ${JSON.stringify({ shots, fade: FADE, version, frame: FRAME, tv: TV })});
    </script>
  </body>
</html>
`;
mkdirSync("build/cuts", { recursive: true });
writeFileSync(`build/cuts/${cutName}-${tone}.html`, page);
console.log(`${cutName}, ${tone}: ${shots.length} shots, ${total.toFixed(1)} s`);
