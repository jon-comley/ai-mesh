# Model Selection — Command Generation (REAPER + Lights)

Which model to run for **command/control generation** — i.e. turning natural language
("set tempo to 120", "warm dim glow in the living room") into reliable **tool calls**
for the REAPER and lighting capabilities.

This is a **different axis** from [`beelink-model-guide.md`](../beelink-model-guide.md),
which ranks models by general quality + raw throughput. For *control*, the dominant
factor is **tool-calling reliability** — a model that "narrates" a tool call as plain
text instead of emitting a structured call is useless no matter how clever or fast it is.

## Scope & caveats

- **Inference runs on pi1, beelink1, and the Mac.** OmniLink1 is **controller-only**
  (`nodes/omnilink1.env` → `NODE_FEATURES=reaper`, no `llm`); the project never schedules
  inference on it.
- **Mac node is not yet configured.** The repo plans a **Mac mini M4 (48 GB unified)**
  (~end July 2026). A Mac Studio (M-Max/Ultra, 32–192 GB unified) would have equal-or-greater
  capacity — figures below assume a 48 GB+ Apple-Silicon / Metal node. Confirm the real
  chip/RAM when it joins.
- **tok/s are beelink1 (Radeon 780M) decode figures** from the model guide. The Mac (Metal)
  is materially faster; pi1 (CPU-only Pi 5) is far slower.

## Ranking — best first for command generation

| # | Model | Why (for tool/command gen) | Size Q4 | ~tok/s¹ | pi1 | beelink1 | Mac |
|---|-------|----------------------------|---------|---------|-----|----------|-----|
| 1 | **qwen2.5:7b** | Gold standard — native function-calling, reliable for REAPER control. Best reliability-vs-speed balance. ⭐ | 4.7 GB | 17.9 | ❌ | ✅ | ✅ |
| 2 | **qwen3:8b** | Excellent tool-calling + better intent disambiguation; run with thinking off for snappy commands | 5.0 GB | 16.6 | ❌ | ✅ | ✅ |
| 3 | **qwen3:4b** | Reliable Qwen tool-calling **and** fast — snappiest solid option, ideal for simple lights commands | 2.5 GB | 30 | ⚠️ slow | ✅ | ✅ |
| 4 | **qwen2.5:14b / qwen3:14b** | Most accurate on ambiguous / multi-step commands; slower. Overkill for "lights off" | 9 GB | ~10 | ❌ | ✅ | ✅ |
| 5 | **phi4:14b** | Strong, decent tools, but stricter-JSON tool format less reliable than Qwen, and slow | 8.9 GB | 9.0 | ❌ | ✅ | ✅ |
| 6 | **mistral:7b** | Has tool support, fast-ish; less consistent than Qwen on edge cases | 4.1 GB | 18.6 | ❌ | ✅ | ✅ |
| 7 | **qwen2.5:1.5b** | The **pi1 pick** — small/fast, reliable enough for *simple* lights; weak on complex REAPER | ~1 GB | fast | ✅ | ✅ | ✅ |
| 8 | **llama3.2:3b** | Tool-capable + very fast, but small → flakier on anything non-trivial | 2.0 GB | 36.7 | ⚠️ slow | ✅ | ✅ |
| 9 | **gemma3:4b** | ⚠️ **Avoid for commands** — no native function-calling; observed emitting tool calls as plain text rather than structured calls (2026-06-25). Fine for chat, bad for control | 2.8 GB | 29.5 | ⚠️ | ✅ | ✅ |
| 10 | **deepseek-r1:8b / 14b** | Reasoning/CoT model — slow, verbose, not tool-optimised. Wrong tool for snappy commands | 5–9 GB | ~10–17 | ❌ | ✅ | ✅ |
| — | **qwen2.5:32b** | Best accuracy, but **only the Mac fits it** (20 GB > beelink's 16 GB UMA) | ~20 GB | — | ❌ | ❌ | ✅ |

¹ decode tok/s on beelink1's 780M (see `beelink-model-guide.md`). ✅ runs well · ⚠️ runs but slow/marginal · ❌ won't fit / impractical.

## Measured on beelink1, 2026-09-12 — and the 14B lost on *correctness*

The ranking above was argued from model-family behaviour. It has now been run.
`reaper-bench.ps1` posts the real `build_system_prompt` shape — the twelve
REAPER tools, `light_command`, a device list — and checks whether the reply
parses as JSON, not merely how fast it arrives.

| Case | `qwen2.5:7b` | `qwen2.5:14b` |
|---|---|---|
| "turn the studio lights off" | 3.3 s, valid | 9.0 s, **invalid JSON** |
| "start recording" | 1.9 s, valid | 2.2 s, valid |
| "add a guitar track, arm it, and set the tempo to 96" | 3.2 s, valid | 4.7 s, **wrong key** |
| "dim the studio lamp to 20% and stop playback" | 3.1 s, valid | 6.2 s, invented target |
| Decode | **17.5 tok/s** | 9.1 tok/s |

**The 14B's multi-step failure is fatal rather than cosmetic.** It emitted
`{"name":"reaper_add_track", …}` where `intent.rs:376` reads `call["tool"]` and
`is_tool_call` requires `v.get("tool")` — so the reply is discarded and the
command silently does nothing. It also produced `"Studio Lamp [Studio]"`,
inventing a target by folding the room tag into the name, which the system
prompt forbids in as many words, and `"20%"` where the schema wants a number.

**So "bump to a 14b Qwen for trickier multi-step intents", above, is wrong on
this hardware and stays only as the record of what we believed.** The 7B went
four for four including the compound cross-domain case, at half the latency.
Bigger is not more obedient: Qwen2.5-7B-Instruct is tuned hard for this exact
format, and the 14B spends its extra parameters on reasoning this task does not
need while losing the schema discipline it does.

**The one drift worth knowing** is the 7B answering "stop playback" with
`reaper_action: "stop"` rather than `reaper_transport`. It works —
`named_action_id` maps `stop` to 1007 — but it is the place to look first if
transport ever behaves oddly.

Harness: `C:\Users\jonno\reaper-bench.ps1` on beelink1, `-ModelFile <name.gguf>`.

## The function-calling specialists, measured — 2026-09-13

Jon asked for the obvious follow-up: if a general instruct model follows the
schema this well, would a model *built* for tool calling do better? Three were
pulled from HuggingFace and run through the same `reaper-bench.ps1` cases,
alongside Qwen3-8B with thinking off — the way `llama.rs` actually runs it.

| Model | Valid JSON | Right key (`"tool"`) | Real targets | "stop playback" | Decode |
|---|---|---|---|---|---|
| **`qwen2.5:7b`** | **4/4** | **4/4** | **yes** | `reaper_action` ⚠️ | 17.5 t/s |
| `qwen3:8b` (`/no_think`) | 4/4 | 4/4 | yes | `reaper_transport` ✅ | 16.8 t/s |
| `xLAM-2-8b-fc-r` | 3/4 | 3/4 | **invented `"Studio"`** | `reaper_transport` ✅ | 17.2 t/s |
| `watt-tool-8B` | **1/4** | — | invented `"Studio"` | `reaper_transport` ✅ | 17.5 t/s |
| `Hammer2.1-7b` | 4/4 | 4/4 | invented once, real once | `reaper_transport` ✅ | 17.7 t/s |
| `qwen2.5:14b` (2026-09-12) | 2/4 | 3/4 | invented | — | 9.1 t/s |

**Every specialist reverted to the format it was trained on, and that is the
whole result.** xLAM slid back to OpenAI-style `{"name": …, "arguments": …}` on
the multi-call reply — the same `"name"` key that sank the 14B, which
`intent.rs:376` discards — and produced malformed JSON doing it. watt-tool
answered three of four in Python call syntax, `light_command(args={…})`, which
is how BFCL scores function calling and not what this prompt asks for. Neither
is a bad model. Both are fighting a custom JSON shape written into a system
prompt, and a heavily instruction-tuned generalist follows *the prompt's* format
better than a specialist follows its own.

**Two things that would change this ranking**, recorded so they are not
rediscovered:

- **Moving ai-mesh to a native tools API** — structured `tools` in the request
  rather than a JSON shape described in prose. That is the format the
  specialists were trained on, and the table could invert.
- **Qwen3-8B depends on `/no_think` being applied.** Without it the same
  multi-step case took 18.6 s and returned nothing. It is the one model here
  that routes to the correct transport tool, but if that flag ever regresses,
  command latency quadruples silently.

**Hammer2.1-7b is the exception that proves the pattern.** It is a fine-tune of
Qwen2.5-7B — the same base as the winner — so it already speaks the format the
prompt describes: all four replies valid, all four using `"tool"`, nesting
correct, and it routes "stop playback" to `reaper_transport` where `qwen2.5:7b`
drifts to `reaper_action`. Its two slips are the ones that matter more,
though: it sent `"Studio"` as the target for "turn the studio lights off" — a
room name, not a device, so that command has nowhere to go — and it sent `arm`
as the string `"true"` where the schema says boolean.

**That string works only by accident, and the accident is a latent bug.**
`intent.rs:1276` reads `args["arm"].as_bool().unwrap_or(true)`: a string is not a
bool, `as_bool()` returns `None`, and the default happens to be `true`. So the
same line turns `"arm": "false"` — "add a track but don't arm it" — into an
**armed** track, silently, for any model that stringifies booleans. Worth
fixing in the parser regardless of which model is loaded: accept `"true"` and
`"false"` as strings, and default to *not* arming when the value is present but
unreadable.

`qwen2.5:7b` stays the pick, and it is already `DEFAULT_MODEL` on beelink1: its
only miss, `reaper_action: "stop"`, still reaches REAPER because
`named_action_id` maps it, whereas an invented target reaches nothing. Hammer was
the one to retry under a native tools API — done 2026-09-13, below: it failed
there, at the parser.

## Native tool calling, measured — 2026-09-13

The follow-up recorded above: does passing tools in the request's `tools`
field, instead of describing them in the system prompt, change the ranking?
`scripts/bench/reaper-bench-tools.ps1` sends the same four cases, the same
twelve tools as JSON Schema, and the same devices to `llama-server` b9444 with
`--jinja`, and scores only **structured `tool_calls`**. Text that looks like a
call is a fail, because ai-mesh couldn't use it. The prompt-mode bench
(`reaper-bench.ps1`) was re-run the same night on the same build, so the two
columns compare directly. `run-reaper-bench.ps1 -Mode tools|prompt` repeats
either.

| Model | Native: structured calls | Native detail | Prompt mode, same night |
|---|---|---|---|
| **`qwen2.5:7b`** | **4/4** | both studio lights; "stop playback" → `reaper_transport` ✅; `arm` a real boolean; real targets | parses 4/4, but one light only, "stop" → `reaper_action` ⚠️, add-track args nested under `properties` |
| `qwen3:8b` (`/no_think`) | 4/4 | one lamp; stop → `reaper_transport`; real targets | 4/4, same choices |
| `qwen2.5:14b` | 4/4 | **invented `"Studio Lamp [Studio]"`**; 8.8 t/s | 3/4; `"name"` key; same invented target |
| `Hammer2.1-7b` | **0/4** | calls came back as text in a code fence, unparsed; `"Studio"` target invented | 4/4 parse; `"Studio"`; `arm` as `"true"` |
| `xLAM-2-8b-fc-r` | **0/4** | calls as text `[{"name","arguments"}]`, unparsed; invented a `reaper_arm_track` tool and `"Studio"` | 3/4; `"name"` key; `"Studio"` |
| `watt-tool-8B` | **0/4** | no calls at all; answered in prose, twice refusing ("no devices listed that can record") | 1/4; Python call syntax |

**The ranking does not flip. It sharpens.** The Qwen models are the ones native
tools help, and the specialists are the ones they fail:

- **The specialists fail in `llama-server`'s parser, not in their answers.**
  Hammer's text was the right calls in the right order apart from the invented
  target, and xLAM's was nearly so. Both write their calls in their own trained
  formats, which b9444 doesn't recognise for those chat templates, so nothing
  reaches `tool_calls`. That's a llama.cpp support gap, and a newer build could
  change it. watt-tool is a template mismatch more broadly: it stopped calling
  tools at all.
- **`qwen2.5:7b` is better natively, on exactly its known weak point.** "Stop
  playback" now routes to `reaper_transport` instead of drifting to
  `reaper_action`. "Studio lights" turns off both studio lights instead of one.
  Booleans stay booleans, and args come back flat, so `normalize_tool_args` has
  nothing to lift.
- **The cost is a first-request overhead, not the second call.** The lights case
  took 7.5 s against 2.8 s. But it's the first request after each model loads,
  and it was 2–3 s slower in native mode for **every** model, including Hammer
  and xLAM, whose native reply was a single call as text. After that first
  request, native and prompt are within about 0.5 s on every case, for every
  model except watt-tool, and generation speed is unchanged. A long-running
  server pays that cost once, not per command. That part is inferred from this
  run, not measured separately.
- **The 14B still invents targets** and still decodes at half speed. Nothing here
  revives it.

**Speeds, same night, beelink1.** Seconds per case. Decode is the average
generation speed. The lights case is the first request after each model loads.

| Model | Mode | lights | transport | multi-step | cross-domain | avg | decode t/s | load |
|---|---|---|---|---|---|---|---|---|
| `qwen2.5:7b` | native | 7.5 | 1.5 | 3.3 | 3.5 | 4.0 | 16.6 | 3 s |
| `qwen2.5:7b` | prompt | 2.8 | 1.3 | 3.2 | 3.1 | 2.6 | 17.6 | 3 s |
| `qwen3:8b` | native | 5.1 | 1.6 | 3.7 | 3.9 | 3.6 | 16.4 | 4 s |
| `qwen3:8b` | prompt | 3.2 | 1.4 | 3.6 | 3.5 | 2.9 | 16.7 | 4 s |
| `xLAM-2-8b` | native | 5.9 | 1.4 | 3.9 | 3.2 | 3.6 | 16.7 | 3 s |
| `xLAM-2-8b` | prompt | 2.9 | 1.3 | 3.1 | 3.1 | 2.6 | 17.2 | 3 s |
| `watt-tool-8B` | native | 1.7 | 2.0 | 5.8 | 2.5 | 3.0 | 17.1 | 3 s |
| `watt-tool-8B` | prompt | 2.5 | 1.3 | 2.1 | 2.2 | 2.0 | 17.3 | 3 s |
| `Hammer2.1-7b` | native | 5.2 | 1.5 | 3.2 | 3.3 | 3.3 | 17.4 | 3 s |
| `Hammer2.1-7b` | prompt | 2.8 | 1.3 | 3.2 | 3.1 | 2.6 | 17.6 | 3 s |
| `qwen2.5:14b` | native | 12.1 | 2.7 | 6.3 | 6.7 | 7.0 | 8.8 | 8 s |
| `qwen2.5:14b` | prompt | 9.1 | 2.2 | 4.7 | 6.2 | 5.5 | 9.2 | 7 s |

watt-tool's native times are for prose, not calls, so they don't compare.

**Four cases is a small sample.** This says native tools are worth trying for
Qwen. It isn't proof on real voice traffic.

**If ai-mesh adopts it, it goes behind a switch**, with the prompt format staying
the default. `llama-server` needs `--jinja`, `intent.rs` reads `tool_calls`
instead of searching text, and turning the switch off must restore exactly
today's behaviour. Hammer and xLAM are worth re-running only after a llama.cpp
upgrade.

## Native tool calling on the REAL prompt — 2026-09-13

The four-case bench above used a cut-down prompt and twelve tools. Before
making native tool calling the default, `scripts/bench/reaper-bench-real.ps1`
ran what ai-mesh actually sends: the system prompts and full tool set exported
from `intent.rs` (`dump_intent_bench_inputs`), real-format device and sensor
lines, a warm-up request first (as the coordinator now does on Ready), and
seven cases. Scored as the coordinator would act: right tools, real targets,
and **plain text with no calls** for a state question.

| Model | Mode | Score | Failures | Avg time (7 cases) |
|---|---|---|---|---|
| **`qwen3:8b`** | **native** | **7/7** | — | 3.6 s |
| `qwen3:8b` | prompt | 6/7 | "what's on?" → **toggled all six lights** | 4.0 s |
| `qwen2.5:7b` | native | 5/7 | "stop playback" → `music_control` pause; "what's on?" → **turned a light on** | 2.9 s |
| `qwen2.5:7b` | prompt | **7/7** | — | 2.5 s |

**This reverses the four-case result for `qwen2.5:7b`.** With the full prompt
it is the better model in prompt mode and the worse one natively, and its
native failure on a state question *acts* — a light switches on — rather than
just answering badly. **`qwen3:8b` native is the only combination with no
failures**, and the only one that answered "what's on?" correctly and fully.

**One run each, at production sampling (temperature 0.4).** A single failure
can be noise. Repeat runs are the next check before choosing a default.

**Warm-up:** the throwaway request takes 12–16 s (the full prompt and tools
being processed once). After it, the first real case took 2.1–2.9 s instead
of the 5–7.5 s seen without it.

## mac1 (M4 Max) on the real prompt — 2026-09-13

Same bench (`scripts/bench/reaper_bench_real.py`, the Python port), same
inputs, same llama.cpp build (b9444, Metal), warm-up first, one run each.
Model files are byte-identical to beelink1's (SHA-256 checked).

| Model | Native | Prompt | Avg per command | Decode |
|---|---|---|---|---|
| **`qwen2.5:14b`** | **7/7** | **7/7** | 1.2 s | ~44 t/s |
| `qwen3:8b` | 7/7 | 6/7 ("what's on?" → three `get_climate` calls) | 0.7–0.8 s | ~73 t/s |
| `qwen2.5:7b` | 5/7 (same two failures as beelink1) | 7/7 | 0.5–0.6 s | ~77–85 t/s |
| `xLAM-2-8b-fc-r` | 1/7 | 6/7 | 0.7–0.8 s | — |
| `Hammer2.1-7b` | 1/7 | 5/7 | 0.6 s | — |
| `watt-tool-8B` | 1/7 | 2/7 | 0.3–0.4 s | — |

**`qwen2.5:14b` is the pick for mac1, and the only model with no failures in
either mode.** Its state answer was also factually right, where
`qwen2.5:7b`'s prompt-mode answer said the Kitchen Pendant was off when it was
on. At 1.2 s a command on the Mac it is still faster than any 7–8B on
beelink1 (2.5–3.6 s). It is loaded on mac1 as of 2026-09-13; the coordinator
picks the largest Ready model, so intents route to it.

**This overturns the four-case result for the 14B** ("invented targets, half
speed"): on the real prompt, with real device lines, it chose real targets every
time. The cut-down bench was too small to judge it.

**mac1 is 4–6× beelink1 per command** for the same model (qwen3:8b native
0.8 s against 3.6 s; warm-up 5 s against 16 s), in line with its memory
bandwidth (546 GB/s).

## Practical picks per machine

- **beelink1** (main compute) → **`qwen2.5:7b`** for control. Drop to **`qwen3:4b`** for
  snappier lights; bump to a **14b Qwen** for trickier multi-step intents.
- **pi1** → **`qwen2.5:1.5b`** — fine for lights, not for complex REAPER.
- **Mac** (when it lands) → **`qwen2.5:14b`** daily driver, or **`qwen2.5:32b`** for max
  accuracy (only node that fits 32b; Metal makes the big Qwens genuinely fast).

## Why Qwen dominates here

The Qwen2.5 / Qwen3 instruct families are explicitly trained for function-calling and emit
clean, schema-valid tool calls consistently. Phi-4, Mistral and Llama-3.x support tools but
are less consistent on strict JSON tool-call format. Gemma has no native function-calling
(prompt-coaxed only) and slips into prose. Reasoning models (DeepSeek-R1) add chain-of-thought
latency and verbosity that hurt snappy command turnaround. For *control*, reliability of the
structured call beats every other property — hence the ranking above departs from the
general-quality ordering in `beelink-model-guide.md`.
