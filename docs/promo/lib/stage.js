// Builds the promo's stage and animates it from a recorded timeline.
//
// Every visual state is its own element, shown and hidden by opacity
// tweens at absolute times: the renderer seeks frames out of order, so
// nothing may depend on a callback having run earlier.
(function () {
  const FPS = 60;
  // A command takes this long to cross the link before the TV reacts,
  // and the panel this long to change. The mock answers at once; a TV
  // doesn't.
  const CROSS = 0.4;
  const FADE = 0.45;

  const LOCK_ICON = {
    open: '<svg viewBox="0 0 24 24" fill="none" stroke="#fcfcfc" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><rect x="4" y="11" width="16" height="10" rx="2"/><path d="M8 11V7a4 4 0 0 1 7.5-2"/></svg>',
    closed: '<svg viewBox="0 0 24 24" fill="none" stroke="#141618" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><rect x="4" y="11" width="16" height="10" rx="2"/><path d="M8 11V7a4 4 0 0 1 8 0v4"/></svg>',
  };

  const frame = (t) => Math.round(t * FPS) / FPS;

  function el(parent, cls, html) {
    const node = document.createElement("div");
    node.className = cls;
    if (html) node.innerHTML = html;
    parent.appendChild(node);
    return node;
  }

  // A set of chips of which one shows at a time.
  function chips(parent, variants, lit) {
    const box = el(parent, "chips");
    const nodes = {};
    for (const [key, text] of Object.entries(variants)) {
      nodes[key] = el(box, "chip" + (lit.includes(key) ? " lit" : ""), text);
    }
    return nodes;
  }

  function show(tl, nodes, key, at, instant) {
    for (const [name, node] of Object.entries(nodes)) {
      const opacity = name === key ? 1 : 0;
      if (instant) tl.set(node, { opacity }, at);
      else tl.to(node, { opacity, duration: 0.25 }, at);
    }
  }

  function buildTv(stage, spec) {
    const label = el(stage, "label");
    label.style.left = spec.x + "px";
    label.style.top = spec.y - 170 + "px";
    el(label, "name", spec.name);
    const state = chips(label, { active: "on", screenoff: "screen off", standby: "off" }, ["active"]);

    const tv = el(stage, "tv");
    Object.assign(tv.style, { left: spec.x + "px", top: spec.y + "px", width: spec.w + "px", height: spec.h + "px" });
    const glow = el(tv, "glow");
    el(tv, "neck");
    el(tv, "foot");
    el(tv, "bezel");
    const screen = el(tv, "screen");
    const desktop = el(screen, "desktop");
    const scale = spec.w / 2000;
    for (const [x, y, w, h] of [[150, 120, 880, 560], [800, 340, 840, 520]]) {
      const win = el(desktop, "window");
      Object.assign(win.style, { left: x * scale + "px", top: y * scale + "px", width: w * scale + "px", height: h * scale + "px" });
    }
    el(desktop, "panel");
    const dark = el(screen, "dark");
    const standby = el(tv, "standby");
    return { state, glow, dark, standby };
  }

  function buildPc(stage, spec) {
    const label = el(stage, "label");
    label.style.left = spec.x + "px";
    label.style.top = spec.y - 170 + "px";
    el(label, "name", "PC");
    const state = chips(
      label,
      { unlocked: "unlocked", locked: "locked", asleep: "asleep", streaming: "streaming" },
      ["locked", "streaming"],
    );

    const pc = el(stage, "pc");
    Object.assign(pc.style, { left: spec.x + "px", top: spec.y + "px", width: spec.w + "px", height: spec.h + "px" });
    el(pc, "case");
    for (const top of [180, 230, 280]) el(pc, "slot").style.top = top + "px";
    const power = el(pc, "power");
    const badge = el(pc, "badge");
    const fill = el(badge, "locked-fill");
    const open = el(badge, "icon", LOCK_ICON.open);
    const closed = el(badge, "icon", LOCK_ICON.closed);
    return { state, power, fill, open, closed };
  }

  // What the PC shows for an action, if the action changes it.
  function pcState(e) {
    if (e.do === "cli" && e.args[0] === "stream") return e.args[1] === "start" ? "streaming" : "unlocked";
    return { lock: "locked", unlock: "unlocked", sleep: "asleep", wake: "unlocked", return: "unlocked" }[e.do];
  }

  // The stage for one TV and the PC, animated from `entries` (a shot's
  // timeline). `spec.beats`, when given, is the time each of the shot's
  // actions lands on: everything the stack did in answer to an action
  // keeps its distance from it. Returns a GSAP timeline for the caller
  // to place.
  function stage(root, entries, spec) {
    const tl = gsap.timeline();
    const tv = buildTv(root, spec.tv);
    const pc = buildPc(root, spec.pc);
    // The link a command crosses: Wi-Fi waves for a network TV, a cable
    // with its plugs for an HDMI-CEC one.
    const linkY = spec.pc.y + spec.pc.h / 2;
    const linkX = spec.tv.x + spec.tv.w;
    const linkW = spec.pc.x - linkX;
    const wireless = spec.link !== "hdmi";
    const link = el(root, wireless ? "air" : "link");
    Object.assign(link.style, { left: linkX + "px", top: linkY + "px", width: linkW + "px" });
    const waves = [];
    let pulse = null;
    if (wireless) {
      // From the PC toward the TV, each wave a little larger.
      for (let i = 0; i < 4; i++) {
        const wave = el(link, "wave");
        const size = 90 + i * 70;
        Object.assign(wave.style, { right: 40 + i * 95 + "px", width: size + "px", height: size + "px", marginTop: -size / 2 + "px" });
        waves.push(wave);
      }
    } else {
      el(link, "plug tv-end");
      el(link, "plug pc-end");
      pulse = el(link, "pulse");
    }
    // The device a stream goes to, lit while the stream runs.
    const handheld = spec.handheld ? el(root, "handheld", '<div class="grip"></div><div class="view"><div class="lit"></div></div><div class="grip"></div>') : null;
    const lit = handheld ? handheld.querySelector(".lit") : null;
    if (handheld) {
      Object.assign(handheld.style, { left: spec.pc.x - 260 + "px", top: spec.pc.y + spec.pc.h + 110 + "px" });
      el(handheld, "tag", "handheld");
    }
    if (spec.wordmark) el(root, "wordmark", spec.wordmark);

    // Where the shot begins: the last state published before its first
    // action, shown from the first frame.
    const first = entries.find((e) => e.kind === "action");
    const origin = first ? first.t : 0;
    let power = "active";
    for (const e of entries) {
      if (e.t > origin) break;
      if (e.kind === "state" && e.tv === spec.tv.name && e.key === "state/power") power = e.value.state;
    }
    show(tl, tv.state, power, 0, true);
    show(tl, pc.state, "unlocked", 0, true);
    tl.set(tv.dark, { opacity: power === "active" ? 0 : 1 }, 0);
    tl.set(tv.glow, { opacity: power === "active" ? 1 : 0 }, 0);
    tl.set(tv.standby, { opacity: power === "standby" ? 1 : 0 }, 0);
    tl.set(pc.closed, { opacity: 0 }, 0);
    if (pulse) tl.set(pulse, { opacity: 0 }, 0);
    if (lit) tl.set(lit, { opacity: 0 }, 0);

    // The moment the TV may react: once the last command has crossed.
    let arrives = 0;
    // How far the current action was moved from its recorded time.
    let beat = 0;
    let shift = spec.beats ? spec.beats[0] - (first.t - origin) : 0;
    for (const e of entries) {
      if (e.t < origin) continue;
      const acts = e.kind === "action" && pcState(e);
      if (acts && spec.beats) {
        shift = spec.beats[beat] - (e.t - origin);
        beat += 1;
      }
      const at = frame(e.t - origin + shift);
      if (e.kind === "action") {
        const state = pcState(e);
        if (!state) continue;
        show(tl, pc.state, state, at);
        const locked = state === "locked" || state === "asleep";
        tl.to(pc.fill, { opacity: locked ? 1 : 0, duration: 0.25 }, at);
        tl.to(pc.closed, { opacity: locked ? 1 : 0, duration: 0.25 }, at);
        tl.to(pc.open, { opacity: locked ? 0 : 1, duration: 0.25 }, at);
        tl.to(pc.power, { opacity: state === "asleep" ? 0.25 : 1, duration: 0.4 }, at);
        if (lit) tl.to(lit, { opacity: state === "streaming" ? 1 : 0, duration: 0.4 }, at);
        // A stream ending sends the TV nothing: it stays dark until
        // someone is back.
        if (e.do === "cli" && e.args[1] === "stop") continue;
        // The command crosses to the TV.
        if (pulse) {
          tl.fromTo(pulse, { x: 0, opacity: 1 }, { x: -linkW, opacity: 1, duration: CROSS, ease: "power1.inOut", immediateRender: false }, at);
          tl.to(pulse, { opacity: 0, duration: 0.1 }, at + CROSS);
        }
        waves.forEach((wave, i) => {
          const step = (CROSS / waves.length) * i;
          tl.to(wave, { opacity: 1, duration: 0.1 }, at + step);
          tl.to(wave, { opacity: 0.18, duration: 0.3 }, at + step + 0.14);
        });
        arrives = at + CROSS;
      } else if (e.kind === "state" && e.tv === spec.tv.name && e.key === "state/power") {
        const state = e.value.state;
        if (state === power) continue;
        power = state;
        const when = Math.max(at, arrives);
        show(tl, tv.state, state, when + FADE / 2);
        tl.to(tv.dark, { opacity: state === "active" ? 0 : 1, duration: FADE, ease: "power2.out" }, when);
        tl.to(tv.glow, { opacity: state === "active" ? 1 : 0, duration: FADE }, when);
        tl.to(tv.standby, { opacity: state === "standby" ? 1 : 0, duration: 0.3 }, when + FADE);
      }
    }
    return tl;
  }

  window.Promo = { stage, el };
})();
