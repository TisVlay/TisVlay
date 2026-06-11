#!/usr/bin/env python3
"""
Capture baseline cargo build times for the tisvlay workspace.

Runs each measurement inside `nix develop` so the toolchain matches what
contributors / CI use. Drops `target/debug` and `target/llvm-cov-target`
between cold steps; uses a unique-marker file edit (not just `touch`) for
incremental steps so cargo's content-hash fingerprint actually invalidates.

Outputs (under bench-results/baseline/):
  - results.json           : structured per-step measurements
  - results.txt            : human-readable summary table
  - timings/<label>.html   : `cargo --timings=html` reports
  - logs/<label>.{stdout,stderr}.txt : full output per step
"""

import json
import shutil
import subprocess
import sys
import time
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
RESULTS = REPO / "bench-results" / "baseline"
TARGET = REPO / "target"
NIX_PREFIX = ["nix", "develop", "--command"]


def log(msg):
    print(msg, flush=True)


def cold_drop():
    """Drop target dirs that would otherwise carry cached artifacts.

    Uses `rm -rf` rather than `shutil.rmtree` because IDE / rust-analyzer
    background tasks can race against the deletion (target/debug/deps
    files mid-write) and `rm -rf` tolerates that better.
    """
    for sub in ("debug", "llvm-cov-target", "cargo-timings"):
        p = TARGET / sub
        if p.exists():
            log(f"  rm -rf {p}")
            subprocess.run(["rm", "-rf", str(p)], check=True)


def force_rebuild(rel_path: str):
    """Append a unique marker comment to invalidate cargo's fingerprint.

    Returns a revert callable that restores the original contents.
    """
    p = REPO / rel_path
    original = p.read_text()
    marker = f"\n// bench-marker {time.time_ns()}\n"
    p.write_text(original + marker)
    log(f"  poked {rel_path}")
    return lambda: p.write_text(original)


def run_step(cmd, label, save_timings=False):
    log(f"\n==[{label}]==")
    log(f"  cmd: {' '.join(cmd)}")
    full = NIX_PREFIX + cmd
    log_dir = RESULTS / "logs"
    timings_dir = RESULTS / "timings"
    log_dir.mkdir(parents=True, exist_ok=True)
    timings_dir.mkdir(parents=True, exist_ok=True)

    start = time.monotonic()
    proc = subprocess.run(
        full,
        cwd=REPO,
        capture_output=True,
        text=True,
    )
    elapsed = time.monotonic() - start

    (log_dir / f"{label}.stdout.txt").write_text(proc.stdout)
    (log_dir / f"{label}.stderr.txt").write_text(proc.stderr)

    if save_timings:
        timings_html = TARGET / "cargo-timings" / "cargo-timing.html"
        if timings_html.exists():
            shutil.copy(timings_html, timings_dir / f"{label}.html")
            log(f"  saved timings -> {timings_dir / f'{label}.html'}")
        else:
            log(f"  (no cargo-timing.html produced)")

    log(f"  exit={proc.returncode}  elapsed={elapsed:.1f}s ({elapsed/60:.2f} min)")
    if proc.returncode != 0:
        log("  stderr tail:")
        for line in proc.stderr.splitlines()[-12:]:
            log(f"    {line}")

    return {
        "label": label,
        "cmd": cmd,
        "exit": proc.returncode,
        "elapsed_s": round(elapsed, 2),
    }


def main():
    RESULTS.mkdir(parents=True, exist_ok=True)
    log(f"results dir: {RESULTS}")
    log(f"start: {time.strftime('%Y-%m-%d %H:%M:%S')}")
    measurements = []
    overall_start = time.monotonic()

    build_cmd_timings = [
        "cargo", "build",
        "--profile=test",
        "--workspace",
        "--all-targets",
        "--all-features",
        "--timings",
    ]

    # 1. Cold full build (matches `just build`, with timings).
    cold_drop()
    measurements.append(run_step(build_cmd_timings, "01-cold-build", save_timings=True))

    # 2. Worst-case incremental: invalidate the bottom crate.
    revert = force_rebuild("crates/tisvlay-core/src/lib.rs")
    try:
        measurements.append(run_step(
            build_cmd_timings, "02-incr-after-core-edit", save_timings=True,
        ))
    finally:
        revert()

    # 3. Best-case incremental: invalidate a leaf bin.
    revert = force_rebuild("crates/tisvlay/src/main.rs")
    try:
        measurements.append(run_step(
            build_cmd_timings, "03-incr-after-leaf-edit", save_timings=True,
        ))
    finally:
        revert()

    # 4. Cold `just check` — full prek pipeline (deny + fmt + build + clippy + test).
    cold_drop()
    measurements.append(run_step(["just", "check"], "04-cold-just-check"))

    # 5. Cold `just test-ci` — coverage instrumentation, the CI bottleneck.
    cold_drop()
    measurements.append(run_step(["just", "test-ci"], "05-cold-just-test-ci"))

    overall_elapsed = time.monotonic() - overall_start

    # Resolve toolchain version inside the dev shell for the summary.
    rustc_v = subprocess.run(
        NIX_PREFIX + ["rustc", "--version"],
        cwd=REPO, capture_output=True, text=True,
    ).stdout.strip()

    summary = {
        "rustc": rustc_v,
        "host": "32-core, 188 GB RAM, NixOS",
        "overall_elapsed_s": round(overall_elapsed, 2),
        "measurements": measurements,
    }
    (RESULTS / "results.json").write_text(json.dumps(summary, indent=2))

    lines = [
        f"rustc:    {summary['rustc']}",
        f"host:     {summary['host']}",
        f"total:    {overall_elapsed/60:.2f} min",
        f"finished: {time.strftime('%Y-%m-%d %H:%M:%S')}",
        "",
        f"{'label':<30} {'exit':<5} {'sec':<10} {'min':<6}",
        "-" * 60,
    ]
    for m in measurements:
        lines.append(
            f"{m['label']:<30} {m['exit']:<5} {m['elapsed_s']:<10} {m['elapsed_s']/60:.2f}"
        )
    text = "\n".join(lines) + "\n"
    (RESULTS / "results.txt").write_text(text)
    log("\n" + text)


if __name__ == "__main__":
    sys.exit(main())
