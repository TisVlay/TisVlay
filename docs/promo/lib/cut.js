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
        pc: { x: 2960, y: 680, w: 400, h: 760 },
        beats: shot.beatTimes,
      });
      tl.add(child, shot.start);
    },

    window(layer, shot, cut) {
      frame(layer, shot.box, cut.frame.bar, shot.title, shot.last);
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
      const pad = el(remote, "pad");
      const press = el(pad, "press");
      for (const top of [640, 760, 880]) el(remote, "key").style.top = top + "px";
      for (const at of shot.keys) {
        tl.fromTo(press, { opacity: 0 }, { opacity: 1, duration: 0.08, immediateRender: false }, shot.start + at);
        tl.to(press, { opacity: 0, duration: 0.3 }, shot.start + at + 0.12);
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
