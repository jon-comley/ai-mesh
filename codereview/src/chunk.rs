//! Splitting the files under review into feature-sized chunks.
//!
//! A chunk is what one worker reads in one go: some target files (under
//! review) plus the files they import (context). The size budget comes from
//! the receiving worker's context, so on mac1 (256k context) a chunk can hold a
//! whole page, the services it calls and the companion-repo code they import —
//! which is what it takes to see a bug like the invoice that bills the cheapest
//! quote option, spread over three files in two repos.

use crate::{SourceFile, estimate_tokens};
use std::collections::{BTreeMap, HashMap, HashSet};

/// An import prefix that points into another repo, e.g. dashboard's
/// `@app/lib/money` → `guv:src/lib/money`.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ImportAlias {
    pub prefix: String,
    pub repo: String,
    pub dir: String,
}

/// One unit of review work: indexes into the planner's file list.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Chunk {
    pub targets: Vec<usize>,
    pub context: Vec<usize>,
    pub tokens: usize,
}

/// A file larger than this is cut in the prompt. A generated file or a huge
/// fixture would otherwise eat a chunk on its own.
pub const MAX_FILE_CHARS: usize = 120_000;

/// Prompt cost of one file: numbered content (about six extra characters a
/// line), plus its diff when it is a target.
pub fn file_cost(f: &SourceFile) -> usize {
    let content_len = f.content.len().min(MAX_FILE_CHARS);
    let lines = f.content[..floor_char_boundary(&f.content, content_len)]
        .lines()
        .count();
    let numbered = content_len + lines * 6;
    let diff = f.diff.as_deref().map(estimate_tokens).unwrap_or(0);
    numbered.div_ceil(7) * 2 + diff + 20
}

pub(crate) fn floor_char_boundary(s: &str, mut i: usize) -> usize {
    if i >= s.len() {
        return s.len();
    }
    while !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}

/// The import specifiers in a JS/TS file, and `mod`/`use crate::` in Rust.
/// A line scanner rather than a parser: it only has to be right often enough
/// to pull the right neighbours in as context, and wrong guesses cost nothing
/// because a specifier that resolves to no known file is dropped.
pub fn import_specifiers(path: &str, content: &str) -> Vec<String> {
    let mut out = Vec::new();
    if path.ends_with(".rs") {
        for line in content.lines() {
            let t = line.trim_start();
            let t = t.strip_prefix("pub ").unwrap_or(t);
            if let Some(rest) = t.strip_prefix("mod ") {
                if let Some(name) = rest.strip_suffix(';') {
                    out.push(format!("mod:{}", name.trim()));
                }
            } else if let Some(rest) = t.strip_prefix("use crate::") {
                let first: String = rest
                    .chars()
                    .take_while(|c| c.is_alphanumeric() || *c == '_')
                    .collect();
                if !first.is_empty() {
                    out.push(format!("crate:{first}"));
                }
            }
        }
        return out;
    }
    for line in content.lines() {
        for marker in ["from ", "import(", "require(", "import "] {
            let mut search = line;
            while let Some(pos) = search.find(marker) {
                let after = &search[pos + marker.len()..];
                let after = after.trim_start();
                if let Some(q) = after.chars().next().filter(|c| *c == '\'' || *c == '"') {
                    let body = &after[1..];
                    if let Some(end) = body.find(q) {
                        let spec = &body[..end];
                        if !spec.is_empty() && !out.iter().any(|s| s == spec) {
                            out.push(spec.to_string());
                        }
                    }
                }
                search = &search[pos + marker.len()..];
            }
        }
    }
    out
}

fn normalize_path(p: &str) -> String {
    let mut parts: Vec<&str> = Vec::new();
    for seg in p.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            s => parts.push(s),
        }
    }
    parts.join("/")
}

fn dir_of(path: &str) -> &str {
    path.rsplit_once('/').map(|(d, _)| d).unwrap_or("")
}

const JS_EXTS: [&str; 6] = ["ts", "tsx", "js", "jsx", "mjs", "cjs"];

fn js_candidates(base: &str) -> Vec<String> {
    let mut c = vec![base.to_string()];
    // ESM-style TS imports name the emitted `.js`; the source is `.ts`/`.tsx`.
    if let Some(stem) = base.strip_suffix(".js") {
        c.push(format!("{stem}.ts"));
        c.push(format!("{stem}.tsx"));
    }
    for ext in JS_EXTS {
        c.push(format!("{base}.{ext}"));
    }
    for ext in JS_EXTS {
        c.push(format!("{base}/index.{ext}"));
    }
    c
}

/// Resolve one file's imports to `(repo, path)` pairs that `exists` knows
/// about. Bare package imports (`react`, `firebase/firestore`) resolve to
/// nothing, which is the point: only the repo's own code is context.
pub fn resolve_imports(
    file: &SourceFile,
    aliases: &[ImportAlias],
    exists: &dyn Fn(&str, &str) -> bool,
) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    let push = |repo: &str, path: String, out: &mut Vec<(String, String)>| {
        if !out.iter().any(|(r, p)| r == repo && *p == path) {
            out.push((repo.to_string(), path));
        }
    };
    let dir = dir_of(&file.path);
    for spec in import_specifiers(&file.path, &file.content) {
        if let Some(name) = spec.strip_prefix("mod:") {
            for cand in [format!("{dir}/{name}.rs"), format!("{dir}/{name}/mod.rs")] {
                let cand = normalize_path(&cand);
                if exists(&file.repo, &cand) {
                    push(&file.repo, cand, &mut out);
                    break;
                }
            }
            continue;
        }
        if let Some(name) = spec.strip_prefix("crate:") {
            // The crate root is the nearest `src` directory above the file.
            if let Some(idx) = file.path.rfind("src/") {
                let root = &file.path[..idx + 3];
                for cand in [format!("{root}/{name}.rs"), format!("{root}/{name}/mod.rs")] {
                    if exists(&file.repo, &cand) {
                        push(&file.repo, cand, &mut out);
                        break;
                    }
                }
            }
            continue;
        }
        let (repo, base) = if spec.starts_with("./") || spec.starts_with("../") {
            (file.repo.clone(), normalize_path(&format!("{dir}/{spec}")))
        } else if let Some(a) = aliases.iter().find(|a| spec.starts_with(&a.prefix)) {
            let rest = &spec[a.prefix.len()..];
            (a.repo.clone(), normalize_path(&format!("{}/{rest}", a.dir)))
        } else {
            continue;
        };
        if let Some(hit) = js_candidates(&base).into_iter().find(|c| exists(&repo, c)) {
            push(&repo, hit, &mut out);
        }
    }
    out
}

struct UnionFind(Vec<usize>);

impl UnionFind {
    fn find(&mut self, x: usize) -> usize {
        let mut r = x;
        while self.0[r] != r {
            r = self.0[r];
        }
        let mut c = x;
        while self.0[c] != r {
            let next = self.0[c];
            self.0[c] = r;
            c = next;
        }
        r
    }
    fn union(&mut self, a: usize, b: usize) {
        let (ra, rb) = (self.find(a), self.find(b));
        if ra != rb {
            self.0[rb] = ra;
        }
    }
}

/// Plan chunks within `budget` tokens each.
///
/// Targets that import each other or share a directory form one group, and a
/// group stays together where it fits. Targets take at most 70% of a chunk;
/// the rest is filled with the files they import, nearest first, so every
/// chunk shows the code its targets call. A target bigger than a whole chunk
/// still gets a chunk of its own, cut at [`MAX_FILE_CHARS`] in the prompt.
pub fn plan_chunks(files: &[SourceFile], aliases: &[ImportAlias], budget: usize) -> Vec<Chunk> {
    let index: HashMap<(String, String), usize> = files
        .iter()
        .enumerate()
        .map(|(i, f)| ((f.repo.clone(), f.path.clone()), i))
        .collect();
    let exists = |repo: &str, path: &str| index.contains_key(&(repo.to_string(), path.to_string()));
    let imports: Vec<Vec<usize>> = files
        .iter()
        .map(|f| {
            resolve_imports(f, aliases, &exists)
                .into_iter()
                .filter_map(|k| index.get(&k).copied())
                .collect()
        })
        .collect();

    let targets: Vec<usize> = (0..files.len()).filter(|&i| files[i].target).collect();
    if targets.is_empty() {
        return Vec::new();
    }
    let mut uf = UnionFind((0..files.len()).collect());
    let mut by_dir: HashMap<(String, &str), usize> = HashMap::new();
    for &t in &targets {
        let key = (files[t].repo.clone(), dir_of(&files[t].path));
        match by_dir.get(&key) {
            Some(&first) => uf.union(first, t),
            None => {
                by_dir.insert(key, t);
            }
        }
        for &imp in &imports[t] {
            if files[imp].target {
                uf.union(t, imp);
            }
        }
    }
    let mut groups: BTreeMap<String, Vec<usize>> = BTreeMap::new();
    for &t in &targets {
        let root = uf.find(t);
        groups
            .entry(files[root].display_name())
            .or_default()
            .push(t);
    }

    let target_budget = budget * 7 / 10;
    let mut chunks = Vec::new();
    let mut current: Vec<usize> = Vec::new();
    let mut current_cost = 0usize;
    let flush = |current: &mut Vec<usize>, current_cost: &mut usize, chunks: &mut Vec<Chunk>| {
        if current.is_empty() {
            return;
        }
        chunks.push(fill_context(
            std::mem::take(current),
            *current_cost,
            files,
            &imports,
            budget,
        ));
        *current_cost = 0;
    };
    for group in groups.values() {
        let group_cost: usize = group.iter().map(|&t| file_cost(&files[t])).sum();
        // Start a fresh chunk for a group that fits whole but not beside the
        // current one, so a feature is not split just because of what
        // happened to come before it.
        if !current.is_empty()
            && current_cost + group_cost > target_budget
            && group_cost <= target_budget
        {
            flush(&mut current, &mut current_cost, &mut chunks);
        }
        for &t in group {
            let cost = file_cost(&files[t]);
            if !current.is_empty() && current_cost + cost > target_budget {
                flush(&mut current, &mut current_cost, &mut chunks);
            }
            current.push(t);
            current_cost += cost;
        }
    }
    flush(&mut current, &mut current_cost, &mut chunks);
    chunks
}

fn fill_context(
    targets: Vec<usize>,
    targets_cost: usize,
    files: &[SourceFile],
    imports: &[Vec<usize>],
    budget: usize,
) -> Chunk {
    let in_chunk: HashSet<usize> = targets.iter().copied().collect();
    let mut context = Vec::new();
    let mut seen = in_chunk.clone();
    let mut tokens = targets_cost;
    // Breadth first: direct imports of the targets, then theirs.
    let mut frontier: Vec<usize> = targets.clone();
    for _depth in 0..2 {
        let mut next = Vec::new();
        for &f in &frontier {
            for &imp in &imports[f] {
                if seen.insert(imp) {
                    next.push(imp);
                }
            }
        }
        for &c in &next {
            let cost = file_cost(&files[c]);
            if tokens + cost <= budget {
                context.push(c);
                tokens += cost;
            }
        }
        frontier = next;
    }
    Chunk {
        targets,
        context,
        tokens,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn f(repo: &str, path: &str, content: &str, target: bool) -> SourceFile {
        SourceFile {
            repo: repo.into(),
            path: path.into(),
            content: content.into(),
            diff: None,
            target,
        }
    }

    #[test]
    fn finds_es_imports_requires_and_dynamic_imports() {
        let src = "import { a } from './a';\nimport b from \"../b/index\";\nconst c = require('./c');\nconst d = await import('./d.js');\nimport './side-effect';\nimport React from 'react';";
        let specs = import_specifiers("src/x.ts", src);
        assert_eq!(
            specs,
            vec![
                "./a",
                "../b/index",
                "./c",
                "./d.js",
                "./side-effect",
                "react"
            ]
        );
    }

    #[test]
    fn finds_rust_mods_and_crate_uses() {
        let src = "mod chunk;\npub mod report;\nuse crate::registry::Registry;\nuse std::fmt;";
        assert_eq!(
            import_specifiers("src/lib.rs", src),
            vec!["mod:chunk", "mod:report", "crate:registry"]
        );
    }

    #[test]
    fn resolves_relative_alias_and_extensionless_imports() {
        let page = f(
            "dashboard",
            "src/pages/JobDetailPage.tsx",
            "import { setJobPricing } from '../services/jobPricing';\nimport { parsePrice } from '@app/lib/money';\nimport x from 'react';",
            true,
        );
        let known = [
            ("dashboard", "src/services/jobPricing.ts"),
            ("guv", "src/lib/money.ts"),
        ];
        let exists = |r: &str, p: &str| known.iter().any(|(kr, kp)| *kr == r && *kp == p);
        let aliases = vec![ImportAlias {
            prefix: "@app/".into(),
            repo: "guv".into(),
            dir: "src".into(),
        }];
        let got = resolve_imports(&page, &aliases, &exists);
        assert_eq!(
            got,
            vec![
                (
                    "dashboard".to_string(),
                    "src/services/jobPricing.ts".to_string()
                ),
                ("guv".to_string(), "src/lib/money.ts".to_string()),
            ]
        );
    }

    #[test]
    fn esm_js_suffix_resolves_to_ts_source() {
        let file = f("r", "src/a.ts", "import { b } from './b.js';", true);
        let exists = |_: &str, p: &str| p == "src/b.ts";
        assert_eq!(
            resolve_imports(&file, &[], &exists),
            vec![("r".to_string(), "src/b.ts".to_string())]
        );
    }

    #[test]
    fn one_chunk_holds_a_small_feature_with_its_imports_as_context() {
        let files = vec![
            f(
                "d",
                "src/pages/A.tsx",
                "import { s } from '../services/s';\nexport const A = 1;",
                true,
            ),
            f("d", "src/services/s.ts", "export const s = 1;", false),
            f("d", "src/unrelated.ts", "export const u = 1;", false),
        ];
        let chunks = plan_chunks(&files, &[], 10_000);
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].targets, vec![0]);
        assert_eq!(chunks[0].context, vec![1]);
    }

    #[test]
    fn splits_when_targets_exceed_seventy_percent_of_budget() {
        let big = "x\n".repeat(1000); // ~2.3k tokens with line numbers
        let files = vec![
            f("d", "a/one.ts", &big, true),
            f("d", "b/two.ts", &big, true),
            f("d", "c/three.ts", &big, true),
        ];
        let chunks = plan_chunks(&files, &[], 4_000);
        assert_eq!(chunks.len(), 3);
        for c in &chunks {
            assert!(c.tokens <= 4_000, "chunk over budget: {}", c.tokens);
        }
    }

    #[test]
    fn a_feature_group_is_kept_together_when_it_fits() {
        let small = "export const v = 1;\n";
        let files = vec![
            f("d", "a/x.ts", &"y\n".repeat(1500), true),
            f("d", "b/p.ts", small, true),
            f("d", "b/q.ts", small, true),
        ];
        let chunks = plan_chunks(&files, &[], 1_500);
        let together = chunks
            .iter()
            .any(|c| c.targets.contains(&1) && c.targets.contains(&2));
        assert!(together, "{chunks:?}");
    }

    #[test]
    fn no_targets_means_no_chunks() {
        let files = vec![f("d", "a.ts", "x", false)];
        assert!(plan_chunks(&files, &[], 1000).is_empty());
    }

    #[test]
    fn context_never_pushes_a_chunk_over_budget() {
        let files = vec![
            f("d", "src/a.ts", "import './b';\nimport './c';", true),
            f("d", "src/b.ts", &"b\n".repeat(400), false),
            f("d", "src/c.ts", &"c\n".repeat(4000), false),
        ];
        let chunks = plan_chunks(&files, &[], 1_000);
        assert_eq!(chunks.len(), 1);
        assert!(chunks[0].tokens <= 1_000);
        assert!(chunks[0].context.contains(&1));
        assert!(!chunks[0].context.contains(&2));
    }

    #[test]
    fn file_cost_caps_huge_files() {
        let huge = f("d", "gen.ts", &"z".repeat(MAX_FILE_CHARS * 3), true);
        assert!(file_cost(&huge) < estimate_tokens(&huge.content));
    }
}
