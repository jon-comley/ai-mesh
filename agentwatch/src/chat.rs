//! Talking to a worker from its tile: one ongoing `claude` conversation per
//! worker, run on lenovo1 in its own clone, with the worker's brief and what its
//! tile shows as context (plans/agent-watch.md, "Chat").
//!
//! This module is the pure part: the context the conversation starts with, and
//! reading `claude -p --output-format stream-json` line by line.

use serde::Serialize;
use serde_json::Value;

/// One thing to show in the chat as it happens.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum Event {
    /// Words from the worker.
    Text {
        text: String,
    },
    /// It is using a tool: "Editing HUB.md".
    Tool {
        text: String,
    },
    /// The answer is finished. `session` resumes the conversation next time.
    Done {
        session: String,
        text: String,
    },
    Error {
        text: String,
    },
}

/// What one line of stream-json says, if it says anything worth showing.
pub fn parse_line(line: &str) -> Vec<Event> {
    let Ok(v) = serde_json::from_str::<Value>(line.trim()) else {
        return Vec::new();
    };
    match v.get("type").and_then(Value::as_str) {
        Some("assistant") => v
            .pointer("/message/content")
            .and_then(Value::as_array)
            .map(|blocks| blocks.iter().filter_map(block).collect())
            .unwrap_or_default(),
        Some("result") => vec![result(&v)],
        _ => Vec::new(),
    }
}

fn block(b: &Value) -> Option<Event> {
    match b.get("type")?.as_str()? {
        "text" => {
            let text = b.get("text")?.as_str()?.trim();
            (!text.is_empty()).then(|| Event::Text {
                text: text.to_string(),
            })
        }
        "tool_use" => Some(Event::Tool {
            text: tool_text(b.get("name")?.as_str()?, b.get("input")),
        }),
        _ => None,
    }
}

fn tool_text(name: &str, input: Option<&Value>) -> String {
    let arg = |k: &str| {
        input
            .and_then(|i| i.get(k))
            .and_then(Value::as_str)
            .unwrap_or("")
    };
    let file = |k: &str| arg(k).rsplit('/').next().unwrap_or("").to_string();
    match name {
        "Read" => format!("Reading {}", file("file_path")),
        "Edit" | "MultiEdit" => format!("Editing {}", file("file_path")),
        "Write" => format!("Writing {}", file("file_path")),
        "Grep" => format!("Searching for {}", arg("pattern")),
        "Glob" => format!("Looking for {}", arg("pattern")),
        "Bash" => {
            let cmd = arg("command");
            let short: String = cmd.chars().take(80).collect();
            if short.len() < cmd.len() {
                format!("Running {short}…")
            } else {
                format!("Running {short}")
            }
        }
        other => format!("Using {other}"),
    }
}

fn result(v: &Value) -> Event {
    let session = v
        .get("session_id")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let text = v
        .get("result")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim()
        .to_string();
    let failed = v.get("is_error").and_then(Value::as_bool).unwrap_or(false)
        || v.get("subtype")
            .and_then(Value::as_str)
            .is_some_and(|s| s != "success");
    if failed {
        let why = if text.is_empty() {
            v.get("subtype")
                .and_then(Value::as_str)
                .unwrap_or("error")
                .to_string()
        } else {
            text
        };
        Event::Error { text: why }
    } else {
        Event::Done { session, text }
    }
}

/// The standing instructions a chat starts with: who it is (its brief), what
/// Jon can see on its tile right now, and how a chat differs from a run.
pub fn context(
    name: &str,
    brief: Option<&str>,
    tile: &str,
    today: &str,
    memory: Option<&Memory>,
) -> String {
    let brief = brief.unwrap_or("(This worker has no brief.)");
    let memory = memory.map(|m| m.context()).unwrap_or_default();
    format!(
        "You are the {name} worker, talking with Jon live from your tile on the agent watch page. \
This is a conversation, not a scheduled run: answer what he asks, do what he asks while he \
watches, and keep replies short.

How Jon wants things written: never use an em dash (use a comma, a colon or a full stop); short, \
say a thing once, no flannel; UK English; statements for requirements, questions for questions.

Your working directory is a clone of your own repo, on main. Before changing anything run \
`git pull --ff-only`. When you change a file, commit as Jon \
(`git -c user.name=jon -c user.email=joncomley@googlemail.com commit`) with a message starting \
\"{name}:\" and no Co-Authored-By or AI attribution, then `git push origin main`. Write only in \
this repo. Never anything about ThingCo.

When Jon tells you something that should outlast this chat (a decision, an answer, a correction, \
a preference), add it straight away to `conversations/{today}.md` in this repo (create it if \
needed; one short bullet per point, newest last) and commit and push it, so your scheduled runs \
know what was agreed.

What your tile shows Jon right now:
{tile}

{memory}Your standing brief (it governs your scheduled runs; follow its rules here too):
{brief}"
    )
}

/// The shared memory as a chat starts with it: the index and `jon/` only.
#[derive(Debug, Clone, PartialEq)]
pub struct Memory {
    /// The memory clone's path where the chat runs.
    pub path: String,
    pub index: String,
    /// `(file, contents)` for each file in `jon/`.
    pub jon: Vec<(String, String)>,
}

impl Memory {
    fn context(&self) -> String {
        let jon: String = self
            .jon
            .iter()
            .map(|(f, c)| format!("--- {f}\n{}\n", c.trim()))
            .collect();
        format!(
            "Shared memory: {path} (its own git repo, shared by every worker). Below is its index \
and the `jon/` folder, which is all you get up front. Open another file there only when what Jon \
is talking about, or the job, is that file's subject: never read the whole memory, never just in \
case. When Jon tells you something that should outlast this chat and matters beyond you (how he \
wants things done, a decision about the business, a fact about a system), write it to memory: \
update the file that covers it, or add one file in the right folder with `name`, `description` \
and `updated` front matter, change its line in INDEX.md, then `git -C {path} pull --ff-only`, \
commit as Jon and push. Never secrets, never ThingCo.

{index}

{jon}
",
            path = self.path,
            index = self.index.trim(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_text_and_tools_from_an_assistant_line() {
        let line = r#"{"type":"assistant","message":{"content":[
            {"type":"text","text":"Looking now."},
            {"type":"tool_use","name":"Edit","input":{"file_path":"/home/jonno/.ai-mesh/agents/chats/desk/HUB.md"}},
            {"type":"tool_use","name":"Bash","input":{"command":"git pull --ff-only"}},
            {"type":"text","text":"   "}]}}"#;
        assert_eq!(
            parse_line(&line.replace('\n', "")),
            [
                Event::Text {
                    text: "Looking now.".into()
                },
                Event::Tool {
                    text: "Editing HUB.md".into()
                },
                Event::Tool {
                    text: "Running git pull --ff-only".into()
                },
            ]
        );
    }

    #[test]
    fn reads_the_result_line() {
        let ok = r#"{"type":"result","subtype":"success","is_error":false,"result":"Done.","session_id":"abc"}"#;
        assert_eq!(
            parse_line(ok),
            [Event::Done {
                session: "abc".into(),
                text: "Done.".into()
            }]
        );
        let bad =
            r#"{"type":"result","subtype":"error_max_turns","is_error":true,"session_id":"abc"}"#;
        assert_eq!(
            parse_line(bad),
            [Event::Error {
                text: "error_max_turns".into()
            }]
        );
    }

    #[test]
    fn ignores_everything_else() {
        assert!(parse_line(r#"{"type":"system","subtype":"init","session_id":"x"}"#).is_empty());
        assert!(parse_line(r#"{"type":"rate_limit_event"}"#).is_empty());
        assert!(parse_line("not json").is_empty());
    }

    #[test]
    fn long_commands_are_cut() {
        let long = format!(
            r#"{{"type":"assistant","message":{{"content":[{{"type":"tool_use","name":"Bash","input":{{"command":"{}"}}}}]}}}}"#,
            "x".repeat(200)
        );
        let Event::Tool { text } = &parse_line(&long)[0] else {
            panic!()
        };
        assert!(text.ends_with('…') && text.len() < 100);
    }

    #[test]
    fn context_carries_the_brief_the_tile_and_the_rules() {
        let c = context("Desk", Some("BRIEF TEXT"), "TILE TEXT", "2026-11-03", None);
        assert!(c.contains("You are the Desk worker"));
        assert!(c.contains("BRIEF TEXT") && c.contains("TILE TEXT"));
        assert!(c.contains("conversations/2026-11-03.md"));
        assert!(c.contains("never use an em dash"));
        assert!(!c.contains('\u{2014}'));
        assert!(!c.contains("Shared memory"));
    }

    #[test]
    fn context_carries_only_the_memory_index_and_jon() {
        let m = Memory {
            path: "~/.ai-mesh/memory".into(),
            index: "# Memory\n- `money/eas-spend.md`: EAS costs".into(),
            jon: vec![("jon/be-punchy.md".into(), "Keep it short.".into())],
        };
        let c = context("Reviews", None, "TILE", "2026-11-03", Some(&m));
        assert!(c.contains("Shared memory: ~/.ai-mesh/memory"));
        assert!(c.contains("- `money/eas-spend.md`: EAS costs"));
        assert!(c.contains("--- jon/be-punchy.md\nKeep it short."));
        assert!(c.contains("never read the whole memory"));
    }
}
