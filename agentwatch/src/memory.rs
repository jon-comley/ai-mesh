//! The shared memory (`jon-comley/memory`) as the Memory tile shows it: the
//! folders and their titles from `INDEX.md`, each file's front matter, and
//! keeping a file's index line right when Jon edits it on the page.

use serde::Serialize;

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Fact {
    /// `jon/be-punchy.md`
    pub path: String,
    pub name: String,
    pub description: String,
    pub updated: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Folder {
    /// `jon`
    pub name: String,
    /// From the folder's heading in `INDEX.md`.
    pub title: String,
    pub facts: Vec<Fact>,
}

/// A file's `name`, `description` and `updated` front matter.
pub fn front_matter(path: &str, text: &str) -> Fact {
    let mut fact = Fact {
        path: path.to_string(),
        name: path
            .rsplit('/')
            .next()
            .unwrap_or(path)
            .trim_end_matches(".md")
            .to_string(),
        description: String::new(),
        updated: String::new(),
    };
    let Some(rest) = text.strip_prefix("---\n") else {
        return fact;
    };
    let Some((head, _)) = rest.split_once("\n---") else {
        return fact;
    };
    for line in head.lines() {
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        let value = unquote(value.trim());
        match key.trim() {
            "name" => fact.name = value,
            "description" => fact.description = value,
            "updated" => fact.updated = value,
            _ => {}
        }
    }
    fact
}

fn unquote(v: &str) -> String {
    match v.strip_prefix('"').and_then(|v| v.strip_suffix('"')) {
        Some(inner) => inner.replace("\\\"", "\""),
        None => v.to_string(),
    }
}

/// Folder titles from `INDEX.md` headings: `## jon/: How Jon works ...`.
pub fn folder_titles(index: &str) -> Vec<(String, String)> {
    index
        .lines()
        .filter_map(|l| l.strip_prefix("## "))
        .filter_map(|h| {
            let (folder, title) = h.split_once("/:")?;
            Some((folder.trim().to_string(), title.trim().to_string()))
        })
        .collect()
}

/// Groups `facts` by folder, in `INDEX.md`'s order, then any folder the index
/// does not name yet.
pub fn folders(index: &str, mut facts: Vec<Fact>) -> Vec<Folder> {
    facts.sort_by(|a, b| a.path.cmp(&b.path));
    let mut out: Vec<Folder> = folder_titles(index)
        .into_iter()
        .map(|(name, title)| Folder {
            name,
            title,
            facts: Vec::new(),
        })
        .collect();
    for f in facts {
        let folder = f.path.split('/').next().unwrap_or("").to_string();
        match out.iter_mut().find(|d| d.name == folder) {
            Some(d) => d.facts.push(f),
            None => out.push(Folder {
                name: folder.clone(),
                title: String::new(),
                facts: vec![f],
            }),
        }
    }
    out.retain(|d| !d.facts.is_empty());
    out
}

/// A path the page may read or write: `<folder>/<name>.md`, nothing a shell or
/// a `..` could use.
pub fn safe_path(path: &str) -> bool {
    let mut parts = path.split('/');
    let (Some(folder), Some(file), None) = (parts.next(), parts.next(), parts.next()) else {
        return false;
    };
    let word = |w: &str| {
        !w.is_empty()
            && !w.starts_with('.')
            && w.chars()
                .all(|c| c.is_ascii_alphanumeric() || "-_.".contains(c))
    };
    word(folder) && word(file) && file.ends_with(".md") && !path.contains("..")
}

/// `index` with the line for `path` saying `description`, added at the end
/// of its folder's list if it had none.
pub fn set_index_line(index: &str, path: &str, description: &str) -> String {
    let line = format!("- `{path}`: {description}");
    let prefix = format!("- `{path}`:");
    let mut lines: Vec<String> = index.lines().map(str::to_string).collect();
    if let Some(l) = lines.iter_mut().find(|l| l.starts_with(&prefix)) {
        *l = line;
    } else {
        let folder = path.split('/').next().unwrap_or("");
        let heading = format!("## {folder}/:");
        match lines.iter().position(|l| l.starts_with(&heading)) {
            Some(h) => {
                let mut at = h + 1;
                while at < lines.len() && !lines[at].starts_with("## ") {
                    at += 1;
                }
                while at > h + 1 && lines[at - 1].trim().is_empty() {
                    at -= 1;
                }
                lines.insert(at, line);
            }
            None => {
                lines.push(String::new());
                lines.push(format!("## {folder}/:"));
                lines.push(String::new());
                lines.push(line);
            }
        }
    }
    let mut out = lines.join("\n");
    if index.ends_with('\n') {
        out.push('\n');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const INDEX: &str = "# Memory\n\nRules.\n\n## jon/: How Jon works (always read)\n\n- `jon/be-punchy.md`: Short replies\n- `jon/no-em-dashes.md`: No em dashes\n\n## money/: Spend and income\n\n- `money/eas-spend.md`: EAS costs\n";

    #[test]
    fn reads_front_matter() {
        let f = front_matter(
            "jon/be-punchy.md",
            "---\nname: be-punchy\ndescription: \"Jon wants \\\"short\\\" replies\"\nupdated: 2026-09-23\n---\n\nBody.",
        );
        assert_eq!(f.name, "be-punchy");
        assert_eq!(f.description, "Jon wants \"short\" replies");
        assert_eq!(f.updated, "2026-09-23");
        assert_eq!(front_matter("jon/x.md", "no front matter").name, "x");
    }

    #[test]
    fn groups_facts_by_the_index_folders() {
        let facts = vec![
            front_matter("money/eas-spend.md", "---\ndescription: EAS costs\n---\n"),
            front_matter("jon/be-punchy.md", "---\ndescription: Short\n---\n"),
            front_matter("infra/pi1.md", "---\ndescription: pi1\n---\n"),
        ];
        let f = folders(INDEX, facts);
        let names: Vec<_> = f.iter().map(|d| (d.name.as_str(), d.facts.len())).collect();
        assert_eq!(names, [("jon", 1), ("money", 1), ("infra", 1)]);
        assert_eq!(f[0].title, "How Jon works (always read)");
        assert_eq!(f[2].title, "");
    }

    #[test]
    fn only_folder_and_file_paths_are_safe() {
        assert!(safe_path("jon/be-punchy.md"));
        assert!(!safe_path("INDEX.md"));
        assert!(!safe_path("../x/y.md"));
        assert!(!safe_path("jon/.git.md"));
        assert!(!safe_path("jon/a/b.md"));
        assert!(!safe_path("jon/x.txt"));
        assert!(!safe_path("jon/x y.md"));
    }

    #[test]
    fn changes_an_index_line_or_adds_it_to_its_folder() {
        let out = set_index_line(INDEX, "jon/be-punchy.md", "Short replies, said once");
        assert!(
            out.contains("- `jon/be-punchy.md`: Short replies, said once\n- `jon/no-em-dashes.md`")
        );
        assert_eq!(out.lines().count(), INDEX.lines().count());
        let out = set_index_line(INDEX, "jon/new.md", "New fact");
        assert!(out.contains(
            "- `jon/no-em-dashes.md`: No em dashes\n- `jon/new.md`: New fact\n\n## money/"
        ));
        let out = set_index_line(INDEX, "infra/pi1.md", "pi1 wifi");
        assert!(out.ends_with("## infra/:\n\n- `infra/pi1.md`: pi1 wifi\n"));
    }
}
