#!/usr/bin/env python3
"""The real-prompt intent bench, for macOS and Linux nodes.

A port of reaper-bench-real.ps1 (Windows), with the same inputs, cases,
warm-up and scoring, so results from mac1 and beelink1 compare directly.
Standard library only; the stock /usr/bin/python3 on macOS is enough.

    AI_MESH_BENCH_OUT=intent-bench.json cargo test -p coordinator dump_intent_bench_inputs -- --ignored
    ./reaper_bench_real.py --server ~/.ai-mesh/llama.cpp-b9444/build/bin/llama-server \\
        --model ~/.ai-mesh/models/Qwen3-8B-Q4_K_M.gguf --mode native --no-think

Scoring follows what the coordinator does with a reply: the right tools
called, real targets, and plain text (no calls) for a state question.
Native mode reads structured tool_calls first and falls back to the text,
exactly as intent.rs does.
"""
import argparse
import json
import os
import re
import subprocess
import sys
import time
import urllib.request

CONTEXT = """Known devices:
  - Studio Ceiling [Studio]  (online, off)
  - Studio Lamp [Studio]  (online, on, 60% brightness)
  - Kitchen Pendant [Kitchen]  (online, on, 100% brightness, 2700 K)
  - Kitchen Spots [Kitchen]  (online, off)
  - Desk Strip [Office]  (online, off)
  - Office Lamp [Office]  (online, on, 40% brightness)
Known groups (control all members at once): Kitchen Group
Available scenes: Relax, Focus

Known sensors:
  - Bedroom Sensor [Bedroom]: 19.5°C, 52% RH, battery 88%
  - Office Sensor [Office]: 22.1°C, 45% RH, motion detected

"""
TARGETS = {"Studio Ceiling", "Studio Lamp", "Kitchen Pendant", "Kitchen Spots",
           "Desk Strip", "Office Lamp", "Kitchen Group", "Studio", "Kitchen", "Office"}
CASES = [
    ("simple lights", "turn the studio lights off", ["light_command"]),
    ("transport", "start recording", ["reaper_transport"]),
    ("multi-step", "add a guitar track, arm it, and set the tempo to 96", ["reaper_add_track", "reaper_set_tempo"]),
    ("cross-domain", "dim the studio lamp to 20% and stop playback", ["light_command", "reaper_transport"]),
    ("colour", "make the kitchen lights blue", ["light_command"]),
    ("state question", "what's on?", None),  # None: plain text, no calls
    ("light + climate", "turn off the office lamp and tell me the bedroom temperature", ["light_command", "get_climate"]),
]
WARM_UP = "Warm-up request: reply with the single word OK."


def post(port, body, timeout=180):
    req = urllib.request.Request(
        f"http://127.0.0.1:{port}/v1/chat/completions",
        data=json.dumps(body).encode("utf-8"),
        headers={"Content-Type": "application/json"},
    )
    with urllib.request.urlopen(req, timeout=timeout) as r:
        return json.load(r)


def text_calls(txt):
    """A cut of try_parse_tool_calls: strip fences, read consecutive JSON values."""
    clean = txt.replace("```json", " ").replace("```", " ").strip()
    dec, i, out = json.JSONDecoder(), 0, []
    while i < len(clean):
        while i < len(clean) and clean[i].isspace():
            i += 1
        if i >= len(clean):
            break
        try:
            v, i = dec.raw_decode(clean, i)
        except ValueError:
            break
        for el in (v if isinstance(v, list) else [v]):
            if isinstance(el, dict) and "tool" in el and isinstance(el.get("args"), dict):
                out.append((el["tool"], el["args"]))
    return out


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--server", required=True)
    ap.add_argument("--model", required=True)
    ap.add_argument("--mode", choices=["native", "prompt"], default="native")
    ap.add_argument("--inputs", default=os.path.join(os.path.dirname(os.path.abspath(__file__)), "intent-bench.json"))
    ap.add_argument("--port", type=int, default=8093)
    ap.add_argument("--ctx", type=int, default=8192)
    ap.add_argument("--no-think", action="store_true")
    a = ap.parse_args()

    data = json.load(open(a.inputs, encoding="utf-8"))
    system = data["native_system"] if a.mode == "native" else data["prompt_system"]
    if a.no_think:
        system += "\n\n/no_think"
    known_tools = set(data["tool_names"])

    log = open(f"/tmp/reaper-bench-real-{a.port}.log", "w")
    srv = subprocess.Popen([a.server, "--model", a.model, "--host", "127.0.0.1", "--port", str(a.port),
                            "--ctx-size", str(a.ctx), "--n-gpu-layers", "99"], stdout=log, stderr=log)
    try:
        start = time.time()
        while True:
            if srv.poll() is not None:
                print(f"!!! llama-server exited early (code {srv.returncode})")
                return 1
            try:
                with urllib.request.urlopen(f"http://127.0.0.1:{a.port}/health", timeout=2) as r:
                    if json.load(r).get("status") == "ok":
                        break
            except Exception:
                pass
            if time.time() - start > 180:
                print("!!! failed to load within 180s")
                return 1
            time.sleep(1)
        name = os.path.basename(a.model)
        print(f"=== {name} [{a.mode}] loaded in {time.time() - start:.0f}s ===")

        # Warm-up: same system prompt and tools, reply discarded.
        wbody = {"model": "bench", "messages": [{"role": "system", "content": system}, {"role": "user", "content": WARM_UP}],
                 "max_tokens": 8, "temperature": 0, "stream": False}
        if a.mode == "native":
            wbody["tools"] = data["tools"]
        t = time.time()
        try:
            post(a.port, wbody)
            print(f"  (warm-up {time.time() - t:.1f}s)")
        except Exception as e:
            print(f"  (warm-up failed: {e})")

        passed, times = 0, []
        for case, q, want in CASES:
            body = {"model": "bench",
                    "messages": [{"role": "system", "content": system}, {"role": "user", "content": CONTEXT + q}],
                    "max_tokens": 400, "temperature": 0.4, "repeat_penalty": 1.1, "stream": False}
            if a.mode == "native":
                body["tools"] = data["tools"]
            t = time.time()
            try:
                r = post(a.port, body)
            except Exception as e:
                print(f"  [{case}] REQUEST FAILED: {e}")
                continue
            dt = time.time() - t
            times.append(dt)
            msg = r["choices"][0]["message"]
            content = re.sub(r"(?s)<think>.*?</think>", "", msg.get("content") or "").strip()

            calls, source = [], "none"
            for tc in msg.get("tool_calls") or []:
                try:
                    args = json.loads(tc["function"].get("arguments") or "{}")
                except ValueError:
                    continue
                if isinstance(args, dict):
                    calls.append((tc["function"]["name"], args))
                    source = "structured"
            if not calls:
                calls = text_calls(content)
                if calls:
                    source = "text"

            notes = []
            if want is None:
                ok = not calls and bool(content)
                if not ok:
                    notes.append("expected a plain-text answer")
            else:
                names = [c[0] for c in calls]
                missing = [w for w in want if w not in names]
                unknown = [n for n in names if n not in known_tools]
                ok = not missing and not unknown
                if missing:
                    notes.append("missing: " + ", ".join(missing))
                if unknown:
                    notes.append("unknown tool: " + ", ".join(unknown))
                for tool, args in calls:
                    tgt = args.get("target")
                    if tool == "light_command" and tgt and tgt not in TARGETS:
                        ok = False
                        notes.append(f"invented target '{tgt}'")
            passed += ok
            tim = r.get("timings", {})
            print(f"  [{case}] {'PASS' if ok else 'FAIL'}  {dt:.1f}s  prefill {tim.get('prompt_per_second', 0):.0f} t/s"
                  f"  decode {tim.get('predicted_per_second', 0):.1f} t/s  calls={len(calls)} ({source})")
            for tool, args in calls:
                print(f"      -> {tool} {json.dumps(args, separators=(',', ':'))}")
            if not calls and content:
                print("      text: " + content.replace("\n", " "))
            for n in notes:
                print(f"      !! {n}")
        avg = sum(times) / len(times) if times else 0
        print(f"  SCORE {passed}/{len(CASES)}  avg {avg:.1f}s")
        print()
    finally:
        srv.terminate()
        try:
            srv.wait(timeout=30)
        except subprocess.TimeoutExpired:
            srv.kill()
    return 0


if __name__ == "__main__":
    sys.exit(main())
