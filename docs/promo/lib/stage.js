// Draws the promo's stage, a TV and the PC beside it, and plays a
// shot's script on it: how things stand when the shot opens, then what
// happens when (lib/build-cut.mjs writes the script from a recorded
// timeline).
//
// Every visual state is its own element, shown and hidden by opacity
// tweens at absolute times: the renderer seeks frames out of order, so
// nothing may depend on a callback having run earlier.
(function () {
  // A command takes this long to cross between the PC and the TV, and
  // the panel this long to change. The mock answers at once; a TV
  // doesn't.
  const CROSS = 0.4;
  const FADE = 0.45;

  const LOCK_ICON = {
    open: '<svg viewBox="0 0 24 24" fill="none" stroke="#fcfcfc" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><rect x="4" y="11" width="16" height="10" rx="2"/><path d="M8 11V7a4 4 0 0 1 7.5-2"/></svg>',
    closed: '<svg viewBox="0 0 24 24" fill="none" stroke="#141618" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><rect x="4" y="11" width="16" height="10" rx="2"/><path d="M8 11V7a4 4 0 0 1 8 0v4"/></svg>',
  };

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

  function buildTv(stage, spec, inputs) {
    const label = el(stage, "label");
    label.style.left = spec.x + "px";
    label.style.top = spec.y - 170 + "px";
    el(label, "name", spec.name);
    const state = chips(
      label,
      { active: "on", screenoff: "screen off", standby: "off", down: "reconnecting" },
      ["active"],
    );

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
    // What the TV shows on an input that isn't the PC's: something else
    // plugged into it.
    const other = el(screen, "other", '<div class="pad"></div>');
    const dark = el(screen, "dark");
    const standby = el(tv, "standby");
    // The input it is on, named on its bezel.
    const tag = el(tv, "inputs");
    const input = {};
    for (const { id, label: text } of inputs) input[id] = el(tag, "input", text);
    return { state, glow, dark, standby, other, input };
  }

  function buildPc(stage, spec) {
    const label = el(stage, "label");
    label.style.left = spec.x + "px";
    label.style.top = spec.y - 170 + "px";
    el(label, "name", "PC");
    const state = chips(
      label,
      { unlocked: "unlocked", locked: "locked", idle: "idle", asleep: "asleep", streaming: "streaming" },
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

  // The stage for one TV and the PC, playing `script`. Returns a GSAP
  // timeline for the caller to place.
  function stage(root, script, spec) {
    const tl = gsap.timeline();
    const tv = buildTv(root, spec.tv, script.inputs);
    const pc = buildPc(root, spec.pc);
    // The link a command crosses: an antenna on each device for a
    // network TV, a cable with its plugs for an HDMI-CEC one.
    const linkY = spec.pc.y + spec.pc.h / 2;
    const linkX = spec.tv.x + spec.tv.w;
    const linkW = spec.pc.x - linkX;
    const wireless = spec.link !== "hdmi";
    // Each antenna sits at the corner turned away from the other device,
    // clear of both.
    const antenna = (side, x, y) => {
      const fan = el(root, "wifi " + side);
      Object.assign(fan.style, { left: x + "px", top: y + "px" });
      el(fan, "dot");
      return [0, 1, 2].map((i) => {
        const arc = el(fan, "arc");
        const size = 110 + i * 90;
        Object.assign(arc.style, { width: size + "px", height: size + "px", left: -size / 2 + "px", top: -size / 2 + "px" });
        return arc;
      });
    };
    let pcWaves = [];
    let tvWaves = [];
    let pulse = null;
    if (wireless) {
      pcWaves = antenna("pc", spec.pc.x - 40, spec.pc.y + 60);
      tvWaves = antenna("tv", spec.tv.x + spec.tv.w - 30, spec.tv.y - 10);
    } else {
      const link = el(root, "link");
      Object.assign(link.style, { left: linkX + "px", top: linkY + "px", width: linkW + "px" });
      el(link, "plug tv-end");
      el(link, "plug pc-end");
      pulse = el(link, "pulse");
    }
    // Light an antenna's waves in turn: outward when it sends, inward
    // when it receives.
    const ripple = (arcs, at, outward) => {
      const order = outward ? arcs : [...arcs].reverse();
      order.forEach((arc, i) => {
        const when = at + (CROSS / 2 / arcs.length) * i;
        tl.to(arc, { opacity: 1, duration: 0.08 }, when);
        tl.to(arc, { opacity: 0.2, duration: 0.3 }, when + 0.12);
      });
    };
    // A command crossing, to the TV or back to the PC.
    const cross = (at, to) => {
      const [sender, receiver] = to === "tv" ? [pcWaves, tvWaves] : [tvWaves, pcWaves];
      if (wireless) {
        ripple(sender, at, true);
        ripple(receiver, at + CROSS / 2, false);
      } else {
        const [from, end] = to === "tv" ? [0, -linkW] : [-linkW, 0];
        tl.fromTo(pulse, { x: from, opacity: 1 }, { x: end, opacity: 1, duration: CROSS, ease: "power1.inOut", immediateRender: false }, at);
        tl.to(pulse, { opacity: 0, duration: 0.1 }, at + CROSS);
      }
    };
    // The device a stream goes to, lit while the stream runs.
    const handheld = spec.handheld ? el(root, "handheld", '<div class="grip"></div><div class="view"><div class="lit"></div></div><div class="grip"></div>') : null;
    const lit = handheld ? handheld.querySelector(".lit") : null;
    if (handheld) {
      Object.assign(handheld.style, { left: spec.pc.x - 260 + "px", top: spec.pc.y + spec.pc.h + 110 + "px" });
      el(handheld, "tag", "handheld");
    }
    // The TV's remote, whose power key lights when pressed.
    const remote = spec.remote ? el(root, "remote small", '<div class="led"></div><div class="pressed"></div><div class="pad"></div>') : null;
    const pressed = remote ? remote.querySelector(".pressed") : null;
    if (remote) Object.assign(remote.style, { left: spec.tv.x - 300 + "px", top: spec.tv.y + 160 + "px" });
    if (spec.wordmark) el(root, "wordmark", spec.wordmark);

    // How things stand when the shot opens. The PC's input is the one
    // the TV starts on.
    const { initial } = script;
    const home = initial.input;
    const awake = (power) => power === "active";
    const pcLook = (state, at, instant) => {
      const locked = state === "locked" || state === "asleep";
      const set = (node, opacity, duration) => (instant ? tl.set(node, { opacity }, at) : tl.to(node, { opacity, duration }, at));
      show(tl, pc.state, state, at, instant);
      set(pc.fill, locked ? 1 : 0, 0.25);
      set(pc.closed, locked ? 1 : 0, 0.25);
      set(pc.open, locked ? 0 : 1, 0.25);
      set(pc.power, state === "asleep" ? 0.25 : 1, 0.4);
      if (lit) set(lit, state === "streaming" ? 1 : 0, 0.4);
    };
    let power = initial.power;
    // A TV in standby is off the network by nature, not reconnecting.
    show(tl, tv.state, initial.link || power === "standby" ? power : "down", 0, true);
    tl.set(tv.dark, { opacity: awake(power) ? 0 : 1 }, 0);
    tl.set(tv.glow, { opacity: awake(power) ? 1 : 0 }, 0);
    tl.set(tv.standby, { opacity: power === "standby" ? 1 : 0 }, 0);
    tl.set(tv.other, { opacity: 0 }, 0);
    show(tl, tv.input, home, 0, true);
    pcLook(initial.pc, 0, true);
    if (pulse) tl.set(pulse, { opacity: 0 }, 0);
    if (pressed) tl.set(pressed, { opacity: 0 }, 0);

    for (const e of script.events) {
      if (e.kind === "pc") pcLook(e.state, e.at);
      if (e.kind === "send") cross(e.at, e.to);
      if (e.kind === "power") {
        power = e.state;
        show(tl, tv.state, power, e.at + FADE / 2);
        tl.to(tv.dark, { opacity: awake(power) ? 0 : 1, duration: FADE, ease: "power2.out" }, e.at);
        tl.to(tv.glow, { opacity: awake(power) ? 1 : 0, duration: FADE }, e.at);
        tl.to(tv.standby, { opacity: power === "standby" ? 1 : 0, duration: 0.3 }, e.at + FADE);
      }
      if (e.kind === "input") {
        show(tl, tv.input, e.id, e.at);
        tl.to(tv.other, { opacity: e.id === home ? 0 : 1, duration: FADE }, e.at);
      }
      if (e.kind === "link") show(tl, tv.state, e.up ? power : "down", e.at);
      if (e.kind === "remote" && pressed) {
        tl.to(pressed, { opacity: 1, duration: 0.06 }, e.at);
        tl.to(pressed, { opacity: 0, duration: 0.4 }, e.at + 0.2);
      }
    }
    return tl;
  }

  window.Promo = { stage, el };
})();
