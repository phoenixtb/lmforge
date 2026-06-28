#!/usr/bin/env python3
"""
think_bench.py — LMForge reasoning/thinking benchmark harness.

Runs a matrix of (model x prompt x think-mode x repetition) against a running
LMForge daemon's OpenAI endpoint, captures rich per-run metrics (finish reason,
reasoning/answer sizes, special-token leaks, loop/repetition signals, latency,
correctness), and writes everything to a timestamped results dir for later
analysis.

Self-contained: stdlib only (no pip installs). Works on macOS, Linux, Windows
wherever Python 3.8+ and a running LMForge daemon are available.

Run from a fresh clone on any platform:
    1. Build + install LMForge core (engine auto-selected per platform):
         macOS/Linux:  scripts/lmforge.sh install --source local
         Windows:      pwsh scripts/lmforge.ps1 install --source local
    2. Make sure the daemon is up (the installer starts it; verify):
         curl http://127.0.0.1:11430/v1/models
    3. Run the benchmark (pulls the whole candidate matrix if missing):
         python3 tests/bench/think_bench.py --pull-missing
       The machine is fingerprinted automatically — the result dir is named
       <timestamp>__<os>-<arch>-<accel> (e.g. ..__linux-x86_64-cuda,
       ..__darwin-arm64-metal, ..__windows-amd64-cpu) and os/arch/accel/engine
       are recorded in report.md + every CSV row, so runs from different
       machines never collide once committed. No manual tagging needed; pass
       --label only to disambiguate two boxes that resolve to the same slug.
    4. Commit the run's report.md + summary.csv (tracked) and compare the
       aggregate table across platforms. Engine matters: macOS exercises the
       oMLX two-call budget orchestrator + stop-token injection; Linux/Windows
       exercise the llama.cpp <think> rewriter path. Loop/leak numbers are
       therefore expected to differ by platform — that's the point of running
       it everywhere.

Common invocations:
    python3 tests/bench/think_bench.py                 # only installed models
    python3 tests/bench/think_bench.py --pull-missing  # pull candidates first
    python3 tests/bench/think_bench.py --quick         # 1 rep, fewer prompts
    python3 tests/bench/think_bench.py --models phi4:4b:reasoning:4bit gemma3:4b:4bit

Results land in:  tests/bench/results/<timestamp>__<machine-slug>/
    summary.jsonl   one JSON object per run (machine-readable, gitignored)
    summary.csv     flat table for spreadsheets (committed for comparison)
    report.md       human-readable aggregate (committed for comparison)
    raw/*.json      full reasoning+answer text per run (gitignored)
A results/LATEST file points at the newest run dir.

The script flushes after every run, so a partial/interrupted run still leaves
usable data.
"""
from __future__ import annotations

import argparse
import csv
import datetime as dt
import json
import os
import platform
import re
import shutil
import socket
import subprocess
import sys
import time
import urllib.request
import zlib
from collections import Counter
from pathlib import Path

# ---------------------------------------------------------------------------
# Config
# ---------------------------------------------------------------------------
DEFAULT_BASE = os.environ.get("LMFORGE_BASE", "http://127.0.0.1:11430")
CALL_TIMEOUT = 240  # seconds per request
# Results live next to this script (tests/bench/results), not the cwd, so runs
# land in the self-contained bench folder no matter where it's invoked from.
DEFAULT_OUTDIR = str(Path(__file__).resolve().parent / "results")

# Sampling profiles. Thinking profile is the anti-loop set; chat is lighter.
THINK_PROFILE = {
    "temperature": 0.6,
    "top_p": 0.95,
    "top_k": 20,
    "repetition_penalty": 1.2,
    "presence_penalty": 0.3,
}
CHAT_PROFILE = {
    "temperature": 0.7,
    "top_p": 0.9,
    "top_k": 20,
    "repetition_penalty": 1.1,
}
THINK_BUDGET = 1024
THINK_ANSWER_TOKENS = 768
CHAT_MAX_TOKENS = 1024

# Candidate models spanning families. `pull` flag = pull on --pull-missing.
# Reasoning-capable and non-reasoning controls are both included on purpose.
CANDIDATE_MODELS = [
    # family: qwen3.5 (hybrid think toggle)
    {"id": "qwen3.5:2b:4bit", "family": "qwen3.5", "note": "weak hybrid", "pull": True},
    {"id": "qwen3.5:4b:6bit", "family": "qwen3.5", "note": "strong hybrid", "pull": True},
    # family: qwen3 (base + dedicated thinking)
    {"id": "qwen3:1.7b:4bit", "family": "qwen3", "note": "weak", "pull": True},
    {"id": "qwen3:4b:thinking:4bit", "family": "qwen3", "note": "dedicated thinking", "pull": True},
    {"id": "qwen3:8b:4bit", "family": "qwen3", "note": "strong", "pull": True},
    # family: phi4
    {"id": "phi4:4b:reasoning:4bit", "family": "phi4", "note": "dedicated reasoning", "pull": True},
    {"id": "phi4:4b:4bit", "family": "phi4", "note": "instruct control", "pull": True},
    # family: gemma (non-reasoning control)
    {"id": "gemma3:4b:4bit", "family": "gemma3", "note": "instruct control", "pull": True},
    # family: llama (non-reasoning control)
    {"id": "llama3.1:8b:4bit", "family": "llama3.1", "note": "instruct control", "pull": True},
    # family: qwen2.5 (non-reasoning control)
    {"id": "qwen2.5:7b:4bit", "family": "qwen2.5", "note": "instruct control", "pull": True},
]

# Prompts. grader: regex matched (case-insensitive) against the answer text.
# repeats: how many times to repeat this prompt per (model, mode).
PROMPTS = [
    {
        "id": "bat_ball",
        "category": "trick-math",
        "text": "A bat and ball cost $1.10. The bat costs $1 more than the ball. How much is the ball? Reason step by step.",
        "grader": r"(\$?\s*0\.05\b|5\s*cents|five\s*cents)",
        "repeats": 3,
    },
    {
        "id": "simple_add",
        "category": "sanity",
        "text": "What is 2+2? Reason briefly, then give the final answer.",
        "grader": r"\b4\b",
        "repeats": 1,
    },
    {
        "id": "count_r_strawberry",
        "category": "tokenizer-trap",
        "text": "How many times does the letter \"r\" appear in the word \"strawberry\"? Reason step by step, then answer.",
        "grader": r"\b3\b|\bthree\b",
        "repeats": 3,
    },
    {
        "id": "sisters",
        "category": "logic",
        "text": "Alice has 3 brothers and she also has 2 sisters. How many sisters does each of Alice's brothers have? Reason step by step.",
        "grader": r"\b3\b|\bthree\b",
        "repeats": 2,
    },
    {
        "id": "seq_next",
        "category": "pattern",
        "text": "What number comes next in the sequence 2, 6, 12, 20, 30, ? Explain your reasoning.",
        "grader": r"\b42\b|\bforty[- ]?two\b",
        "repeats": 2,
    },
    {
        "id": "primary_colors",
        "category": "instruct-control",
        "text": "List exactly three primary colors, one per line.",
        "grader": r"(?is)red.*(blue|yellow).*(blue|yellow)",
        "repeats": 1,
    },
]

QUICK_PROMPT_IDS = {"bat_ball", "count_r_strawberry", "simple_add"}

SPECIAL_TOKENS = ["<|end|>", "<|assistant|>", "<|im_end|>", "<|im_start|>",
                  "<|eot_id|>", "<|endoftext|>", "<end_of_turn>", "</s>"]


# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------
def _slug(s: str, maxlen: int = 40) -> str:
    """Filesystem/branch-safe lowercase slug."""
    s = re.sub(r"[^A-Za-z0-9._-]+", "-", str(s)).strip("-").lower()
    return s[:maxlen] or "x"


def detect_accelerator(sysname: str, arch: str) -> str:
    """Auto-detect the compute backend so the machine slug is meaningful
    without a manual label. Returns one of: metal / cuda / rocm / vulkan / cpu."""
    # Apple Silicon → Metal/MLX
    if sysname == "Darwin" and arch.lower() in ("arm64", "aarch64"):
        return "metal"
    # NVIDIA CUDA — nvidia-smi present and actually lists a GPU
    if shutil.which("nvidia-smi"):
        try:
            p = subprocess.run(["nvidia-smi", "-L"], capture_output=True,
                               text=True, timeout=8)
            if p.returncode == 0 and "GPU" in p.stdout:
                return "cuda"
        except Exception:
            pass
    # AMD ROCm
    if shutil.which("rocminfo") or shutil.which("rocm-smi"):
        return "rocm"
    # Generic Vulkan offload (e.g. integrated/Intel/Arc via llama.cpp)
    if shutil.which("vulkaninfo"):
        try:
            p = subprocess.run(["vulkaninfo", "--summary"], capture_output=True,
                               text=True, timeout=8)
            if p.returncode == 0 and "GPU" in p.stdout:
                return "vulkan"
        except Exception:
            pass
    return "cpu"


def _cmd_out(args: list[str]) -> str:
    """Run a command, return trimmed stdout or '' on any failure."""
    try:
        p = subprocess.run(args, capture_output=True, text=True, timeout=8)
        if p.returncode == 0:
            return p.stdout.strip()
    except Exception:
        pass
    return ""


def build_provenance() -> dict:
    """Identify the exact build under test so a committed report is never
    ambiguous about which daemon produced it (the #1 thing that bit us when
    comparing platforms — we couldn't tell stale daemons from fresh ones)."""
    here = str(Path(__file__).resolve().parent)
    return {
        # e.g. "lmforge 0.1.5" — the installed CLI/daemon on PATH
        "lmforge_version": _cmd_out(["lmforge", "--version"]),
        # short SHA of the checkout the harness ran from (best-effort)
        "git_sha": _cmd_out(["git", "-C", here, "rev-parse", "--short", "HEAD"]),
        # whether that checkout has uncommitted changes
        "git_dirty": bool(_cmd_out(["git", "-C", here, "status", "--porcelain"])),
    }


def warn_if_stale_daemon(host: dict) -> None:
    """Loudly flag the #1 benchmarking footgun: running an old daemon against a
    new checkout. The daemon's `--version` carries the SHA it was *built* from;
    `git_sha` is the SHA the harness *ran* from. If they differ, the results
    describe a binary that no longer matches the source — exactly the confound
    that invalidated an earlier cross-platform comparison."""
    ver = host.get("lmforge_version") or ""
    checkout = (host.get("git_sha") or "").replace("-dirty", "").strip()
    # daemon version looks like: "lmforge 0.1.5 (08efdff-dirty 2026-06-28)"
    m = re.search(r"\(([0-9a-f]+)", ver)
    built = m.group(1) if m else ""
    if built and checkout and built != checkout:
        print("\n" + "!" * 72)
        print(f"WARNING: daemon was built from {built} but checkout is {checkout}.")
        print("         The running daemon does NOT match this source tree.")
        print("         Rebuild+install before trusting results:")
        print("           scripts/lmforge.sh install --source local   (or .ps1 on Windows)")
        print("!" * 72 + "\n")


def daemon_engine(base: str) -> str:
    """Best-effort active engine id from the daemon (empty string if unknown)."""
    for path in ("/lf/info", "/lf/status", "/lf/engine"):
        try:
            with urllib.request.urlopen(base + path, timeout=5) as r:
                data = json.load(r)
            for k in ("engine", "engine_id", "active_engine"):
                v = data.get(k) if isinstance(data, dict) else None
                if isinstance(v, dict):
                    v = v.get("id") or v.get("name")
                if v:
                    return str(v)
        except Exception:
            continue
    return ""


def host_fingerprint(base: str, label: str | None) -> dict:
    """Identify the machine a run came from so committed results are
    attributable — fully automatic. `label` is only an optional override for
    the slug (e.g. when running two boxes that auto-resolve to the same tag)."""
    sysname = platform.system() or "unknown"        # Darwin / Linux / Windows
    arch = platform.machine() or "unknown"           # arm64 / x86_64 / AMD64
    accel = detect_accelerator(sysname, arch)        # metal / cuda / rocm / vulkan / cpu
    try:
        host = socket.gethostname().split(".")[0]
    except Exception:
        host = "host"
    prov = build_provenance()
    fp = {
        "label": label or "",
        "os": sysname,
        "os_release": platform.release(),
        "os_version": platform.version(),
        "arch": arch,
        "accel": accel,
        "hostname": host,
        "cpu_count": os.cpu_count(),
        "python": platform.python_version(),
        "engine": daemon_engine(base),
        "lmforge_version": prov["lmforge_version"],
        "git_sha": prov["git_sha"] + ("-dirty" if prov["git_dirty"] else ""),
    }
    # Slug for the result dir name. Auto-derived and meaningful by default:
    #   linux-x86_64-cuda, windows-amd64-cuda, darwin-arm64-metal, linux-x86_64-cpu
    # An explicit --label overrides it entirely (e.g. to disambiguate two boxes
    # that resolve to the same os-arch-accel).
    fp["slug"] = _slug(label) if label else "-".join(_slug(p) for p in (sysname, arch, accel))
    return fp


def http_models(base: str) -> dict:
    """Return {id: capabilities} from /v1/models."""
    try:
        with urllib.request.urlopen(base + "/v1/models", timeout=15) as r:
            data = json.load(r)
        return {m["id"]: m.get("capabilities", {}) for m in data.get("data", [])}
    except Exception as e:
        print(f"  ! could not reach {base}/v1/models: {e}", file=sys.stderr)
        return {}


def stream_chat(base: str, model: str, prompt: str, think: bool) -> dict:
    """Single streamed chat request. Returns metrics + text."""
    body = {
        "model": model,
        "messages": [{"role": "user", "content": prompt}],
        "stream": True,
    }
    if think:
        body.update(THINK_PROFILE)
        body["think"] = True
        body["thinking_budget"] = THINK_BUDGET
        body["stream_reasoning_deltas"] = True
        body["max_tokens"] = THINK_BUDGET + THINK_ANSWER_TOKENS
    else:
        body.update(CHAT_PROFILE)
        body["max_tokens"] = CHAT_MAX_TOKENS

    req = urllib.request.Request(
        base + "/v1/chat/completions",
        data=json.dumps(body).encode(),
        headers={"Content-Type": "application/json"},
        method="POST",
    )
    r_deltas = 0
    c_deltas = 0
    reasoning = []
    content = []
    finish = None
    err = None
    t0 = time.time()
    ttfb = None
    try:
        with urllib.request.urlopen(req, timeout=CALL_TIMEOUT) as resp:
            for raw in resp:
                line = raw.decode("utf-8", "replace").strip()
                if not line.startswith("data:"):
                    continue
                payload = line[5:].strip()
                if payload == "[DONE]":
                    break
                try:
                    d = json.loads(payload)
                except json.JSONDecodeError:
                    continue
                if ttfb is None:
                    ttfb = time.time() - t0
                ch = (d.get("choices") or [{}])[0]
                dl = ch.get("delta", {})
                rc = dl.get("reasoning_content")
                cc = dl.get("content")
                if rc:
                    r_deltas += 1
                    reasoning.append(rc)
                if cc:
                    c_deltas += 1
                    content.append(cc)
                if ch.get("finish_reason"):
                    finish = ch["finish_reason"]
    except Exception as e:  # noqa: BLE001
        err = f"{type(e).__name__}: {e}"

    latency = time.time() - t0
    rt = "".join(reasoning)
    ct = "".join(content)
    return {
        "error": err,
        "finish_reason": finish,
        "latency_s": round(latency, 2),
        "ttfb_s": round(ttfb, 2) if ttfb is not None else None,
        "reasoning_deltas": r_deltas,
        "content_deltas": c_deltas,
        "reasoning_chars": len(rt),
        "content_chars": len(ct),
        "reasoning": rt,
        "content": ct,
    }


def loop_metrics(text: str) -> dict:
    """Repetition/degeneracy signals over a text blob."""
    lines = [l.strip() for l in text.splitlines() if len(l.strip()) > 8]
    max_run = 1
    cur = 1
    for i in range(1, len(lines)):
        if lines[i] == lines[i - 1]:
            cur += 1
            max_run = max(max_run, cur)
        else:
            cur = 1
    distinct_ratio = (len(set(lines)) / len(lines)) if lines else 1.0
    top_line_freq = Counter(lines).most_common(1)[0][1] if lines else 0
    # most repeated 6-gram (word level)
    words = re.findall(r"\S+", text)
    top_6gram = 0
    if len(words) >= 6:
        grams = Counter(tuple(words[i:i + 6]) for i in range(len(words) - 5))
        top_6gram = grams.most_common(1)[0][1]
    # compression ratio: lower => more repetitive
    comp = 1.0
    if text:
        raw = text.encode("utf-8")
        comp = round(len(zlib.compress(raw, 6)) / max(len(raw), 1), 3)
    return {
        "max_consecutive_repeat": max_run,
        "distinct_line_ratio": round(distinct_ratio, 3),
        "top_line_freq": top_line_freq,
        "top_6gram_freq": top_6gram,
        "compression_ratio": comp,
    }


def looks_looped(m: dict, full_text: str) -> bool:
    """Heuristic verdict combining the signals."""
    if m["max_consecutive_repeat"] >= 3:
        return True
    if m["top_6gram_freq"] >= 5:
        return True
    if len(full_text) > 400 and m["compression_ratio"] < 0.18:
        return True
    if m["top_line_freq"] >= 4:
        return True
    return False


def pull_model(model_id: str) -> bool:
    print(f"  pulling {model_id} ...", flush=True)
    try:
        p = subprocess.run(["lmforge", "pull", model_id],
                           capture_output=True, text=True, timeout=3600)
        ok = p.returncode == 0
        print(f"    -> {'ok' if ok else 'FAILED'}", flush=True)
        if not ok:
            print(p.stderr[-500:], file=sys.stderr)
        return ok
    except Exception as e:  # noqa: BLE001
        print(f"    -> pull error: {e}", file=sys.stderr)
        return False


# ---------------------------------------------------------------------------
# Main
# ---------------------------------------------------------------------------
def main() -> int:
    ap = argparse.ArgumentParser(description="LMForge reasoning/thinking benchmark")
    ap.add_argument("--base", default=DEFAULT_BASE)
    ap.add_argument("--models", nargs="*", help="explicit model id list (overrides candidates)")
    ap.add_argument("--pull-missing", action="store_true", help="pull configured models that aren't installed")
    ap.add_argument("--quick", action="store_true", help="1 rep, reduced prompt set")
    ap.add_argument("--repeats", type=int, default=None, help="override repeats for every prompt")
    ap.add_argument("--think-only", action="store_true", help="only run think=on (skip think=off)")
    ap.add_argument("--outdir", default=DEFAULT_OUTDIR,
                    help="results root (default: tests/bench/results next to this script)")
    ap.add_argument("--label", default=os.environ.get("BENCH_LABEL"),
                    help="OPTIONAL slug override. By default the machine slug is "
                         "auto-detected as <os>-<arch>-<accel> (e.g. linux-x86_64-cuda); "
                         "only pass this to disambiguate two boxes that resolve the same.")
    args = ap.parse_args()

    base = args.base.rstrip("/")
    host = host_fingerprint(base, args.label)

    # Resolve model set
    if args.models:
        wanted = [{"id": m, "family": m.split(":")[0], "note": "cli"} for m in args.models]
    else:
        wanted = CANDIDATE_MODELS

    installed = http_models(base)
    if not installed:
        print("No daemon / no models reachable. Is `lmforge` running?", file=sys.stderr)
        return 2

    # Pull missing (only those flagged pull or explicitly requested)
    if args.pull_missing:
        for m in wanted:
            if m["id"] not in installed and (m.get("pull") or args.models):
                if pull_model(m["id"]):
                    installed = http_models(base)

    run_models = [m for m in wanted if m["id"] in installed]
    skipped = [m["id"] for m in wanted if m["id"] not in installed]
    if skipped:
        print(f"Skipping not-installed (use --pull-missing): {', '.join(skipped)}")
    if not run_models:
        print("No installed models to run.", file=sys.stderr)
        return 2

    prompts = PROMPTS
    if args.quick:
        prompts = [p for p in PROMPTS if p["id"] in QUICK_PROMPT_IDS]

    # Output dirs — name carries a machine fingerprint so committed results
    # from different platforms (mac/fedora/windows) never collide and are
    # self-identifying: <timestamp>__<slug>
    ts = dt.datetime.now().strftime("%Y%m%d_%H%M%S")
    out = Path(args.outdir) / f"{ts}__{host['slug']}"
    raw_dir = out / "raw"
    raw_dir.mkdir(parents=True, exist_ok=True)
    Path(args.outdir, "LATEST").write_text(str(out.resolve()) + "\n")

    summ_jsonl = (out / "summary.jsonl").open("w")
    csv_f = (out / "summary.csv").open("w", newline="")
    csv_fields = [
        "host", "os", "arch", "accel", "engine", "lmforge_version", "git_sha",
        "model", "family", "note", "prompt", "category", "mode", "rep",
        "finish_reason", "correct", "blank", "looped", "latency_s", "ttfb_s",
        "reasoning_chars", "content_chars", "reasoning_deltas", "content_deltas",
        "leak", "max_consecutive_repeat", "top_6gram_freq", "top_line_freq",
        "distinct_line_ratio", "compression_ratio", "error",
    ]
    csv_w = csv.DictWriter(csv_f, fieldnames=csv_fields)
    csv_w.writeheader()

    # Plan
    total = 0
    plan = []
    for m in run_models:
        caps = installed.get(m["id"], {})
        thinking = bool(caps.get("thinking"))
        modes = ["on"] if (args.think_only and thinking) else (["off", "on"] if thinking else ["off"])
        if args.think_only and not thinking:
            modes = ["off"]
        for p in prompts:
            reps = args.repeats if args.repeats is not None else (1 if args.quick else p["repeats"])
            for mode in modes:
                for rep in range(1, reps + 1):
                    plan.append((m, caps, p, mode, rep))
                    total += 1

    print(f"\n== think_bench ==")
    print(f"host    : {host['slug']}  ({host['os']} {host['os_release']} / "
          f"{host['arch']} / {host['accel']}"
          f"{', engine=' + host['engine'] if host['engine'] else ''})")
    print(f"build   : {host['lmforge_version'] or '?'}  git={host['git_sha'] or '?'}")
    warn_if_stale_daemon(host)
    print(f"base    : {base}")
    print(f"models  : {len(run_models)} ({', '.join(x['id'] for x in run_models)})")
    print(f"prompts : {len(prompts)} | total runs: {total}")
    print(f"outdir  : {out}\n")

    done = 0
    agg = {}  # (model, mode) -> counters
    for (m, caps, p, mode, rep) in plan:
        done += 1
        think = (mode == "on")
        tag = f"{m['id']}__{p['id']}__{mode}__r{rep}"
        # Windows forbids : \ / * ? " < > | in filenames; model ids use colons.
        safe_tag = re.sub(r'[:\\/*?"<>|]', "-", tag)
        print(f"[{done}/{total}] {m['id']:<26} {p['id']:<20} think={mode} rep={rep} ...",
              end="", flush=True)
        res = stream_chat(base, m["id"], p["text"], think)

        full = (res["reasoning"] + "\n" + res["content"]).strip()
        lm = loop_metrics(full)
        leak = any(t in res["content"] for t in SPECIAL_TOKENS)
        looped = looks_looped(lm, full)
        grader = p.get("grader")
        # The user only ever sees `content`. A run that emits reasoning but no
        # content (e.g. thinking budget exhausted mid-<think>, finish=length)
        # is a BLANK answer and must score as a failure — never fall back to
        # grading the reasoning text (that inflates "correct" with answers the
        # user never received).
        blank = not res["content"].strip()
        if grader is None:
            correct = None
        elif blank:
            correct = False
        else:
            correct = bool(re.search(grader, res["content"], re.IGNORECASE))

        row = {
            "host": host["slug"], "os": host["os"], "arch": host["arch"],
            "accel": host["accel"], "engine": host["engine"],
            "lmforge_version": host["lmforge_version"], "git_sha": host["git_sha"],
            "model": m["id"], "family": m.get("family"), "note": m.get("note"),
            "prompt": p["id"], "category": p["category"], "mode": mode, "rep": rep,
            "finish_reason": res["finish_reason"], "correct": correct,
            "blank": blank, "looped": looped,
            "latency_s": res["latency_s"], "ttfb_s": res["ttfb_s"],
            "reasoning_chars": res["reasoning_chars"], "content_chars": res["content_chars"],
            "reasoning_deltas": res["reasoning_deltas"], "content_deltas": res["content_deltas"],
            "leak": leak, "error": res["error"], **lm,
        }
        csv_w.writerow({k: row.get(k) for k in csv_fields})
        csv_f.flush()
        summ_jsonl.write(json.dumps(row) + "\n")
        summ_jsonl.flush()
        (raw_dir / f"{safe_tag}.json").write_text(json.dumps({
            "prompt_text": p["text"], **row,
            "reasoning": res["reasoning"], "content": res["content"],
        }, indent=2), encoding="utf-8")

        # aggregate
        key = (m["id"], mode)
        a = agg.setdefault(key, {"n": 0, "correct": 0, "blank": 0, "looped": 0,
                                 "leak": 0, "length": 0, "errors": 0})
        a["n"] += 1
        if correct:
            a["correct"] += 1
        if blank:
            a["blank"] += 1
        if looped:
            a["looped"] += 1
        if leak:
            a["leak"] += 1
        if res["finish_reason"] == "length":
            a["length"] += 1
        if res["error"]:
            a["errors"] += 1

        flags = []
        if res["error"]:
            flags.append("ERR")
        if looped:
            flags.append("LOOP")
        if leak:
            flags.append("LEAK")
        if blank:
            flags.append("BLANK")
        if correct is True:
            flags.append("ok")
        elif correct is False and not blank:
            flags.append("wrong")
        print(f" {res['finish_reason'] or '?':<7} {res['latency_s']:>5}s "
              f"r/c={res['reasoning_chars']}/{res['content_chars']} {' '.join(flags)}",
              flush=True)

    summ_jsonl.close()
    csv_f.close()

    # Report
    lines = ["# think_bench report", "",
             f"- when: {ts}",
             f"- machine: **{host['slug']}**" + (f" (`{host['label']}`)" if host['label'] else ""),
             f"- os: {host['os']} {host['os_release']} ({host['os_version']})",
             f"- arch: {host['arch']} | accel: {host['accel']} | cpus: {host['cpu_count']} | python: {host['python']}",
             f"- engine: {host['engine'] or 'unknown'}",
             f"- build: {host['lmforge_version'] or 'unknown'} | git: {host['git_sha'] or 'unknown'}",
             f"- hostname: {host['hostname']}",
             f"- base: {base}",
             f"- models: {len(run_models)} | prompts: {len(prompts)} | runs: {total}", "",
             "## Aggregate (model x mode)", "",
             "`correct` = real answers the user saw (blank/length runs score as fail). "
             "`blank` = produced no answer content (e.g. thinking budget exhausted).", "",
             "| model | mode | n | correct | blank | looped | leak | length | err |",
             "|---|---|---|---|---|---|---|---|---|"]
    for (mid, mode), a in sorted(agg.items()):
        lines.append(f"| {mid} | {mode} | {a['n']} | {a['correct']}/{a['n']} | "
                     f"{a['blank']} | {a['looped']} | {a['leak']} | {a['length']} | {a['errors']} |")
    (out / "report.md").write_text("\n".join(lines) + "\n", encoding="utf-8")

    print(f"\nDONE. Results in: {out}")
    print(f"  - {out}/summary.csv")
    print(f"  - {out}/summary.jsonl")
    print(f"  - {out}/report.md")
    print(f"  - raw per-run text in {raw_dir}/")
    print("BENCH_COMPLETE")
    return 0


if __name__ == "__main__":
    sys.exit(main())
