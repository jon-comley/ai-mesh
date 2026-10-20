//! The context note sent with a diff to a reviewer that sees only the diff:
//! per changed file, what it is for, the signatures of what it uses, and the
//! lines that call it. Signatures and call sites, not bodies, so the note is
//! small and little of the code leaves the house, yet "can this be null?" and
//! "does a helper for this already exist?" are answerable.

/// What the note says about one changed file.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct NoteEntry {
    /// `repo/path`.
    pub name: String,
    /// The file's opening comment, if it has one.
    pub purpose: Option<String>,
    /// For each file it imports: `repo/path` and that file's exported signatures.
    pub uses: Vec<(String, Vec<String>)>,
    /// Lines elsewhere that use what it exports: `(path, line, text)`.
    pub called_by: Vec<(String, u32, String)>,
}

/// Longest line kept from a signature or call site.
const MAX_LINE: usize = 160;
/// Signatures kept per imported file.
const MAX_SIGNATURES: usize = 15;
/// The whole note is cut here.
pub const MAX_NOTE_CHARS: usize = 24_000;

fn cut(line: &str) -> String {
    let line = line.trim();
    if line.chars().count() <= MAX_LINE {
        line.to_string()
    } else {
        let mut s: String = line.chars().take(MAX_LINE).collect();
        s.push('…');
        s
    }
}

/// The file's opening comment block (`//`, `/** */` or `#` lines before the
/// first line of code), at most six lines, joined. `None` if there isn't one.
pub fn purpose(content: &str) -> Option<String> {
    let mut lines = Vec::new();
    for line in content.lines() {
        let t = line.trim();
        if t.is_empty() {
            if lines.is_empty() {
                continue;
            }
            break;
        }
        let text = if let Some(rest) = t.strip_prefix("//") {
            rest.trim_start_matches(['/', '!'])
        } else if t.starts_with("/*") || t.starts_with('*') || t.ends_with("*/") {
            t.trim_end_matches("*/")
                .trim_start_matches("/**")
                .trim_start_matches("/*")
                .trim_start_matches('*')
        } else if let Some(rest) = t.strip_prefix('#').filter(|_| !t.starts_with("#!")) {
            rest
        } else {
            break;
        };
        let text = text.trim();
        if !text.is_empty() {
            lines.push(text.to_string());
        }
        if lines.len() == 6 {
            break;
        }
    }
    (!lines.is_empty()).then(|| lines.join(" "))
}

/// The names a JS/TS file exports: functions, consts, classes, types.
pub fn exported_names(content: &str) -> Vec<String> {
    let mut out = Vec::new();
    for line in content.lines() {
        let Some(rest) = line.trim_start().strip_prefix("export ") else {
            continue;
        };
        let rest = rest
            .trim_start_matches("default ")
            .trim_start_matches("async ");
        let rest = [
            "function* ",
            "function ",
            "const ",
            "let ",
            "class ",
            "interface ",
            "type ",
            "enum ",
        ]
        .iter()
        .find_map(|k| rest.strip_prefix(k));
        let Some(rest) = rest else { continue };
        let name: String = rest
            .chars()
            .take_while(|c| c.is_alphanumeric() || *c == '_' || *c == '$')
            .collect();
        if !name.is_empty() && !out.contains(&name) {
            out.push(name);
        }
    }
    out
}

/// A file's exported signatures: each `export` line, cut before its body.
pub fn signatures(content: &str) -> Vec<String> {
    content
        .lines()
        .map(str::trim)
        .filter(|l| l.starts_with("export "))
        .map(|l| {
            let l = l.strip_suffix('{').unwrap_or(l).trim_end();
            cut(l)
        })
        .take(MAX_SIGNATURES)
        .collect()
}

/// The note, entry by entry, cut at [`MAX_NOTE_CHARS`].
pub fn render(entries: &[NoteEntry]) -> String {
    let mut out = String::new();
    for e in entries {
        let mut block = format!("## {}\n", e.name);
        if let Some(p) = &e.purpose {
            block.push_str(&format!("What it is for: {}\n", cut(p)));
        }
        if !e.uses.is_empty() {
            block.push_str("Uses:\n");
            for (file, sigs) in &e.uses {
                block.push_str(&format!("- {file}\n"));
                for s in sigs {
                    block.push_str(&format!("    {s}\n"));
                }
            }
        }
        if e.called_by.is_empty() {
            block.push_str("Called by: nothing found elsewhere in the repo\n");
        } else {
            block.push_str("Called by:\n");
            for (path, line, text) in &e.called_by {
                block.push_str(&format!("- {path}:{line}  {}\n", cut(text)));
            }
        }
        block.push('\n');
        if out.len() + block.len() > MAX_NOTE_CHARS {
            out.push_str("(note cut here: too long)\n");
            break;
        }
        out.push_str(&block);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const FILE: &str = "// Works out what a job costs.\n// Prices are in pence.\n\nimport { round } from './money';\n\nexport function priceOf(job: Job): number {\n  return round(job.total);\n}\nexport const VAT_RATE = 0.2;\nexport async function save(job) {}\nexport default class Pricer {}\nexport type Job = { total: number };\nconst hidden = 1;\n";

    #[test]
    fn reads_the_opening_comment_and_the_exports() {
        assert_eq!(
            purpose(FILE).as_deref(),
            Some("Works out what a job costs. Prices are in pence.")
        );
        assert_eq!(
            purpose("/**\n * Money helpers.\n */\nexport {}").as_deref(),
            Some("Money helpers.")
        );
        assert_eq!(purpose("import x from 'y';"), None);
        assert_eq!(
            exported_names(FILE),
            vec!["priceOf", "VAT_RATE", "save", "Pricer", "Job"]
        );
        assert_eq!(
            signatures(FILE)[0],
            "export function priceOf(job: Job): number"
        );
    }

    #[test]
    fn renders_and_stays_within_its_limit() {
        let e = NoteEntry {
            name: "guv/src/price.ts".into(),
            purpose: purpose(FILE),
            uses: vec![(
                "guv/src/money.ts".into(),
                vec!["export function round(n: number): number".into()],
            )],
            called_by: vec![(
                "src/invoice.ts".into(),
                12,
                "const p = priceOf(job);".into(),
            )],
        };
        let note = render(std::slice::from_ref(&e));
        assert!(note.starts_with("## guv/src/price.ts\nWhat it is for: Works out"));
        assert!(note.contains("    export function round(n: number): number\n"));
        assert!(note.contains("- src/invoice.ts:12  const p = priceOf(job);"));
        let many = vec![e; 1000];
        assert!(render(&many).len() <= MAX_NOTE_CHARS + 40);
        assert!(render(&many).ends_with("(note cut here: too long)\n"));
        let lonely = NoteEntry {
            name: "x".into(),
            ..Default::default()
        };
        assert!(render(&[lonely]).contains("Called by: nothing found"));
    }
}
