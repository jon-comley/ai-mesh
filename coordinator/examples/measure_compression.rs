//! Phase-A measurement harness for prompt compression.
//!
//! Runs the statistical compressor over a corpus of representative prompts —
//! including a multilingual sample and a long (30+ turn) conversation history —
//! and prints the per-sample and average token savings. This is how we decide
//! whether the savings justify wiring the Phase-B cloud gateway, before any API
//! keys are involved.
//!
//! Run with:  `cargo run -p coordinator --example measure_compression`
//! (or `just measure-compression`). Honour `PROMPT_COMPRESS_RATIO` to sweep the
//! target ratio.

use coordinator::compress::{CompressionEngine, compress};

fn long_history() -> String {
    // A 30-turn back-and-forth — the kind of context that actually accrues
    // tokens and where compression should pay off.
    let mut s = String::from(
        "Known devices: kitchen_ceiling, living_lamp_1, living_lamp_2, hall_strip, \
         bedroom_lamp, office_desk, office_shelf, bathroom_spot. Rooms: kitchen, \
         living, hall, bedroom, office, bathroom. Scenes: cozy, focus, movie, \
         sunrise, party.\n",
    );
    let exchanges = [
        (
            "What can you control in the living room?",
            "I can control living_lamp_1 and living_lamp_2, plus any scenes that include them, such as cozy and movie.",
        ),
        (
            "Dim the lamps a bit",
            "I've lowered both living-room lamps to about forty percent brightness.",
        ),
        (
            "Make them warmer",
            "Both lamps are now set to a warmer two thousand seven hundred kelvin tone.",
        ),
        (
            "Now the kitchen",
            "The kitchen ceiling light is on at full brightness with a neutral white tone.",
        ),
        (
            "Too bright, halve it",
            "The kitchen ceiling is now at fifty percent.",
        ),
        (
            "What's the office like?",
            "The office desk and shelf lights are off at the moment.",
        ),
        (
            "Turn them on for work",
            "I've switched on the office desk and shelf lights at a bright, cool tone suited to focus.",
        ),
        (
            "Actually load the focus scene",
            "The focus scene is now active across the office.",
        ),
        (
            "How long until sunset?",
            "Sunset is in roughly two hours and ten minutes from now.",
        ),
        (
            "Schedule sunrise scene for then",
            "I've noted you'd like the sunrise scene around sunset; scheduling isn't wired up yet, so please ask again closer to the time.",
        ),
        (
            "Set the hall to blue",
            "The hall strip is now a saturated blue.",
        ),
        (
            "A little softer",
            "I've reduced the hall strip's brightness so the blue reads softer.",
        ),
        (
            "What scenes do I have?",
            "You have cozy, focus, movie, sunrise, and party scenes available.",
        ),
        (
            "Start movie mode",
            "Movie mode is active: lamps dimmed and warmed, hall lowered.",
        ),
        (
            "Pause that, lights up",
            "I've raised the main lights back to a comfortable level.",
        ),
    ];
    for (u, a) in exchanges {
        s.push_str(&format!("User: {u}\nAssistant: {a}\n"));
    }
    s
}

fn samples() -> Vec<(&'static str, String)> {
    vec![
        (
            "short-command",
            "turn the kitchen lights blue and dim them a little".to_string(),
        ),
        (
            "technical-question",
            "Explain how TCP keepalive works, why it matters for long-lived idle \
             connections behind NAT and load balancers, how the keepalive timer, \
             interval and probe count interact, and what failure modes look like \
             when intermediate devices silently drop idle flows. Cover both the \
             kernel-level socket options and the application-level heartbeats that \
             people often add on top, and when each is the right tool."
                .to_string(),
        ),
        (
            "multilingual",
            "Bonjour, j'aimerais comprendre comment fonctionne la compression de \
             prompts pour les grands modèles de langage. ¿Cómo se preserva el \
             significado mientras se eliminan tokens redundantes? Bitte erkläre \
             auch, warum statistische Filter ohne ein Modell auskommen und \
             trotzdem die wichtigsten Begriffe behalten. 要点を保ちながら、\
             冗長な語を取り除く仕組みを説明してください。"
                .to_string(),
        ),
        ("long-history", long_history()),
    ]
}

fn main() {
    let ratio = std::env::var("PROMPT_COMPRESS_RATIO").unwrap_or_else(|_| "0.5 (default)".into());
    println!("Prompt compression — Phase A measurement");
    println!("Engine: Statistical (compression-prompt)   target ratio: {ratio}\n");
    println!(
        "{:<20} {:>8} {:>8} {:>8} {:>8}  compressed?",
        "sample", "before", "after", "saved", "ratio"
    );
    println!("{}", "-".repeat(72));

    let mut ratio_sum = 0.0f32;
    let mut ratio_n = 0u32;
    let mut total_before = 0usize;
    let mut total_after = 0usize;

    for (name, text) in samples() {
        let out = compress(&text, CompressionEngine::Statistical);
        total_before += out.orig_tokens;
        total_after += out.new_tokens;
        if out.compressed {
            ratio_sum += out.ratio;
            ratio_n += 1;
        }
        println!(
            "{:<20} {:>8} {:>8} {:>8} {:>8.2}  {}",
            name,
            out.orig_tokens,
            out.new_tokens,
            out.tokens_saved(),
            out.ratio,
            if out.compressed {
                "yes"
            } else {
                "no (passthrough)"
            }
        );
    }

    println!("{}", "-".repeat(72));
    let avg_ratio = if ratio_n > 0 {
        ratio_sum / ratio_n as f32
    } else {
        1.0
    };
    let overall = if total_before > 0 {
        total_after as f32 / total_before as f32
    } else {
        1.0
    };
    println!(
        "avg ratio (compressed samples): {avg_ratio:.2}   \
         overall {total_before} -> {total_after} tokens ({overall:.2}, \
         {} saved)",
        total_before.saturating_sub(total_after)
    );
    println!(
        "\nNote: token counts are the crate's ~chars/4 estimate; short prompts \
         below the 1024-byte / 100-token floor pass through unchanged."
    );
}
