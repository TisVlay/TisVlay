// Assembles a cut from the layout lib/build-cut.mjs wrote: one layer
// per shot, cross-faded, with the captions over them. The audio and
// video elements are already on the page; this draws what surrounds
// them.
(function () {
  const { el, stage } = Promo;

  // An image the page already holds, moved to where it is shown.
  function image(id, parent, cls) {
    const img = document.getElementById(id);
    img.className = cls || "";
    parent.appendChild(img);
    return img;
  }

  // A window's frame around a box of content: a title bar in the
  // desktop's style, and the content's last frame behind the clip, which
  // holds when the clip has played out.
  function frame(layer, box, bar, title, still) {
    const win = el(layer, "frame");
    Object.assign(win.style, { left: box.x + "px", top: box.y - bar + "px", width: box.w + "px", height: box.h + bar + "px" });
    const top = el(win, "titlebar");
    top.style.height = bar + "px";
    if (!bar) top.style.display = "none";
    el(top, "title", title);
    el(top, "buttons", "<i></i><i></i><i></i>");
    if (still) {
      const img = image(still, win);
      Object.assign(img.style, { position: "absolute", left: 0, top: bar + "px", width: box.w + "px", height: box.h + "px" });
    }
    return win;
  }

  // A pointer over the shot's clip: it glides to each click and rings.
  function pointer(root, shot, tl) {
    const arrow = el(
      root,
      "pointer",
      '<div class="ring"></div><svg viewBox="0 0 24 24"><path d="M3 2l7.5 19 2.6-7.6L20.7 11z" fill="#fcfcfc" stroke="#141618" stroke-width="1.4" stroke-linejoin="round"/></svg>',
    );
    const ring = arrow.querySelector(".ring");
    const place = (click) => ({ x: shot.box.x + click.x * shot.box.w, y: shot.box.y + click.y * shot.box.h });
    // It waits low in the window, out of the way.
    let from = { x: shot.box.x + shot.box.w * 0.62, y: shot.box.y + shot.box.h * 0.82 };
    tl.set(arrow, from, 0);
    for (const click of shot.clicks) {
      if (click.at < 0 || click.at > shot.duration) continue;
      const to = place(click);
      const travel = Math.min(0.6, Math.max(0.25, click.at));
      tl.fromTo(arrow, from, { ...to, duration: travel, ease: "power2.inOut", immediateRender: false }, shot.start + click.at - travel);
      tl.fromTo(ring, { opacity: 0.9, scale: 0.4 }, { opacity: 0, scale: 1.3, duration: 0.35, immediateRender: false }, shot.start + click.at);
      from = to;
    }
    return arrow;
  }

  const builders = {
    title(layer, shot) {
      const card = el(layer, "card");
      image(shot.banner, card, "banner");
      el(card, "pitch", shot.sentences[0] ? shot.sentences[0].text.replace(/\.$/, "") : "TisVlay");
    },

    end(layer, shot, cut) {
      const card = el(layer, "card");
      el(card, "name", "TisVlay");
      el(card, "version", "v" + cut.version);
      el(card, "links", "tisvlay.readthedocs.io<br>github.com/TisVlay/TisVlay");
    },

    stage(layer, shot, cut, tl) {
      const child = stage(layer, window.__promoTimelines[shot.timeline], {
        tv: { name: shot.tv, x: 480, y: 420, w: 2000, h: 1160 },
        pc: { x: 2960, y: shot.handheld ? 520 : 680, w: 400, h: 760 },
        beats: shot.beatTimes,
        link: shot.link,
        handheld: shot.handheld,
      });
      tl.add(child, shot.start);
    },

    window(layer, shot, cut, tl) {
      frame(layer, shot.box, cut.frame.bar, shot.title, shot.last);
      if (!shot.prompt) return;
      // The TV being paired, beside the window: its prompt comes up when
      // the pairing starts, and "Yes" is picked when someone approves.
      // An illustration: the prompt is the TV's own to draw.
      const x = shot.box.x + shot.box.w + 130;
      const w = 3840 - x - 130;
      const h = Math.round((w * 9) / 16);
      const y = shot.box.y + 260;
      const label = el(layer, "label");
      Object.assign(label.style, { left: x + "px", top: y - 150 + "px" });
      el(label, "name", shot.prompt.name);
      const tv = el(layer, "tv");
      Object.assign(tv.style, { left: x + "px", top: y + "px", width: w + "px", height: h + "px" });
      el(tv, "neck");
      el(tv, "foot");
      el(tv, "bezel");
      const screen = el(tv, "screen");
      el(screen, "desktop");
      const ask = el(screen, "ask", '<div class="question">Allow this PC to connect?</div><div class="answers"><span class="yes">Yes<i></i></span><span>No</span></div>');
      const picked = ask.querySelector(".yes i");
      const from = shot.start + shot.prompt.from + 0.4;
      const to = shot.start + shot.prompt.to;
      tl.set(ask, { opacity: 0 }, 0);
      tl.set(picked, { opacity: 0 }, 0);
      tl.to(ask, { opacity: 1, duration: 0.3 }, from);
      tl.to(picked, { opacity: 1, duration: 0.1 }, to - 0.15);
      tl.to(ask, { opacity: 0, duration: 0.3 }, to + 0.25);
    },

    // A clip playing on the TV, with the remote that drives it.
    tv(layer, shot, cut, tl) {
      const tv = el(layer, "tv");
      Object.assign(tv.style, { left: cut.tv.x + "px", top: cut.tv.y + "px", width: cut.tv.w + "px", height: cut.tv.h + "px" });
      el(tv, "neck");
      el(tv, "foot");
      el(tv, "bezel");
      const screen = el(tv, "screen");
      const img = image(shot.last, screen);
      Object.assign(img.style, { position: "absolute", inset: 0, width: "100%", height: "100%" });
      const label = el(layer, "label");
      label.style.left = cut.tv.x + "px";
      label.style.top = cut.tv.y - 150 + "px";
      el(label, "name", shot.tv);

      const remote = el(layer, "remote");
      el(remote, "led");
      // A direction pad: the arrow pressed lights, so the remote shows
      // which way the focus moves on the screen.
      const pad = el(remote, "pad");
      const lights = {};
      for (const key of ["up", "down", "left", "right", "ok"]) {
        el(pad, "arrow " + key);
        lights[key] = el(pad, "arrow lit " + key);
        tl.set(lights[key], { opacity: 0 }, 0);
      }
      for (const top of [660, 780, 900]) el(remote, "key").style.top = top + "px";
      for (const press of shot.keys) {
        if (press.at < 0 || press.at > shot.duration) continue;
        tl.to(lights[press.key], { opacity: 1, duration: 0.06 }, shot.start + press.at);
        tl.to(lights[press.key], { opacity: 0, duration: 0.35 }, shot.start + press.at + 0.14);
      }
    },

    stills(layer, shot, cut, tl) {
      // Screenshots carry their own title bar.
      const win = frame(layer, cut.frame, 0, "", null);
      const each = shot.duration / shot.images.length;
      shot.images.forEach((src, i) => {
        const img = image(src, win, "still");
        Object.assign(img.style, { top: 0, height: cut.frame.h + "px" });
        if (i > 0) {
          tl.set(img, { opacity: 0 }, 0);
          tl.to(img, { opacity: 1, duration: 0.4 }, shot.start + i * each);
        }
      });
    },
  };

  function cut(root, layout) {
    const tl = gsap.timeline({ paused: true });
    const footage = [...root.querySelectorAll("video.footage")];
    layout.shots.forEach((shot, i) => {
      const layer = el(root, "shot");
      builders[shot.kind](layer, shot, layout, tl);
      // The shot's clip is on the page already, above its layer's frame.
      const clip = footage.find((video) => video.id === "clip-" + shot.index);
      const fading = clip ? [layer, clip] : [layer];
      if (clip) root.appendChild(clip);
      if (shot.clicks && shot.clicks.length) fading.push(pointer(root, shot, tl));
      if (i > 0) {
        tl.set(fading, { opacity: 0 }, 0);
        tl.to(fading, { opacity: 1, duration: layout.fade }, shot.start);
      }
      if (i < layout.shots.length - 1) {
        tl.to(fading, { opacity: 0, duration: layout.fade }, shot.start + shot.duration - layout.fade);
      }
      shot.sentences.forEach((sentence, n) => {
        // The title card shows its pitch itself.
        if (shot.kind === "title" && n === 0) return;
        const caption = el(root, "caption", sentence.text);
        const from = shot.start + sentence.start;
        tl.set(caption, { opacity: 0 }, 0);
        tl.to(caption, { opacity: 1, duration: 0.15 }, from);
        tl.to(caption, { opacity: 0, duration: 0.2 }, from + sentence.duration + 0.1);
      });
    });
    // The timeline runs the cut's whole length, whatever its last tween.
    const last = layout.shots.at(-1);
    tl.set({}, {}, last.start + last.duration);
    return tl;
  }

  window.Promo.cut = cut;
})();
