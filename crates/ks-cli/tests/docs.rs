//! Documentation guards for the two READMEs. They read files only (no Redis, no browser), so they
//! run on every CI build. The READMEs are the same document in two languages: they must keep the
//! same section markers in the same order, stay short, keep installation out of the opening lines,
//! and never link to a file that does not exist.

use std::fs;
use std::path::{Path, PathBuf};

const REQUIRED: [&str; 11] = [
    "example",
    "diagram",
    "why",
    "fit",
    "quickstart",
    "how",
    "rag",
    "remote",
    "status",
    "google",
    "docs",
];

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn read(rel: &str) -> String {
    fs::read_to_string(root().join(rel)).unwrap_or_else(|e| panic!("read {rel}: {e}"))
}

/// `<!-- s:<id> -->` markers, in order, and whether each one sits directly above a `## ` heading.
fn markers(text: &str) -> Vec<(String, bool)> {
    let lines: Vec<&str> = text.lines().collect();
    let mut out = Vec::new();
    for (i, l) in lines.iter().enumerate() {
        let t = l.trim();
        if let Some(id) = t
            .strip_prefix("<!-- s:")
            .and_then(|r| r.strip_suffix(" -->"))
        {
            let above_heading = lines.get(i + 1).is_some_and(|n| n.starts_with("## "));
            out.push((id.to_string(), above_heading));
        }
    }
    out
}

fn h2_count(text: &str) -> usize {
    let mut in_fence = false;
    let mut n = 0;
    for l in text.lines() {
        if l.starts_with("```") {
            in_fence = !in_fence;
        } else if !in_fence && l.starts_with("## ") {
            n += 1;
        }
    }
    n
}

#[test]
fn docs_sections_match() {
    let en = markers(&read("README.md"));
    let ko = markers(&read("README.ko.md"));
    let en_ids: Vec<&str> = en.iter().map(|m| m.0.as_str()).collect();
    let ko_ids: Vec<&str> = ko.iter().map(|m| m.0.as_str()).collect();
    assert_eq!(
        en_ids, ko_ids,
        "README.md and README.ko.md must carry the same section markers in the same order"
    );
    for (name, ms) in [("README.md", &en), ("README.ko.md", &ko)] {
        assert!(
            ms.iter().all(|m| m.1),
            "{name}: every marker must sit directly above a `## ` heading"
        );
    }
    for (name, rel) in [("README.md", "README.md"), ("README.ko.md", "README.ko.md")] {
        assert_eq!(
            h2_count(&read(rel)),
            en.len(),
            "{name}: every `## ` heading needs exactly one marker above it"
        );
    }
}

#[test]
fn docs_required_sections() {
    for rel in ["README.md", "README.ko.md"] {
        let ids: Vec<String> = markers(&read(rel)).into_iter().map(|m| m.0).collect();
        for want in REQUIRED {
            assert!(
                ids.iter().any(|i| i == want),
                "{rel}: missing section marker `{want}`"
            );
        }
    }
}

#[test]
fn readme_line_budget() {
    for rel in ["README.md", "README.ko.md"] {
        let n = read(rel).lines().count();
        assert!(
            (120..=160).contains(&n),
            "{rel}: {n} lines, expected 120 to 160"
        );
    }
}

#[test]
fn docs_no_install_in_first_40_lines() {
    let banned = [
        "cargo install",
        "docker compose",
        "claude plugin",
        "/plugin install",
        "Installation",
        "설치와 설정",
    ];
    for rel in ["README.md", "README.ko.md"] {
        let text = read(rel);
        for (i, l) in text.lines().take(40).enumerate() {
            for b in banned {
                assert!(
                    !l.contains(b),
                    "{rel}:{}: installation detail `{b}` in the opening lines",
                    i + 1
                );
            }
        }
    }
}

#[test]
fn readme_states_ft_hybrid_backlog() {
    for rel in ["README.md", "README.ko.md"] {
        let text = read(rel);
        assert!(
            text.contains("FT.HYBRID"),
            "{rel}: the FT.HYBRID backlog notice is missing"
        );
        assert!(
            text.contains("(v0.2.0)"),
            "{rel}: remote MCP must be labelled (v0.2.0)"
        );
    }
}

#[test]
fn docs_no_local_paths() {
    let mut files = vec!["README.md".to_string(), "README.ko.md".to_string()];
    files.extend(doc_files());
    for rel in files {
        assert!(
            !read(&rel).contains(concat!("/", "Users/")), // split so the literal itself is not a local path
            "{rel}: contains a local path"
        );
    }
}

/// Markdown files under `docs/` (relative paths) plus the root documents that exist.
fn doc_files() -> Vec<String> {
    fn walk(dir: &Path, base: &Path, out: &mut Vec<String>) {
        let Ok(rd) = fs::read_dir(dir) else { return };
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                walk(&p, base, out);
            } else if p.extension().is_some_and(|x| x == "md") {
                out.push(p.strip_prefix(base).unwrap().to_string_lossy().into_owned());
            }
        }
    }
    let base = root();
    let mut out = Vec::new();
    walk(&base.join("docs"), &base, &mut out);
    for f in ["SECURITY.md", "CHANGELOG.md", "ARCHITECTURE.md"] {
        if base.join(f).exists() {
            out.push(f.to_string());
        }
    }
    out
}

/// Relative link targets (`](path)` outside code fences and inline code), fragment stripped.
fn relative_links(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut in_fence = false;
    for l in text.lines() {
        if l.starts_with("```") {
            in_fence = !in_fence;
            continue;
        }
        if in_fence {
            continue;
        }
        let no_code: String = l
            .split('`')
            .enumerate()
            .filter(|(i, _)| i % 2 == 0)
            .map(|(_, s)| s)
            .collect::<Vec<_>>()
            .join(" ");
        let mut rest = no_code.as_str();
        while let Some(i) = rest.find("](") {
            rest = &rest[i + 2..];
            let Some(end) = rest.find(')') else { break };
            let target = rest[..end].split('#').next().unwrap_or("");
            let target = target.split_whitespace().next().unwrap_or("");
            if !target.is_empty() && !target.contains("://") && !target.starts_with("mailto:") {
                out.push(target.to_string());
            }
            rest = &rest[end..];
        }
    }
    out
}

#[test]
fn docs_relative_links_exist() {
    let mut files = vec!["README.md".to_string(), "README.ko.md".to_string()];
    files.extend(doc_files());
    let mut checked = 0;
    for rel in files {
        let dir = Path::new(&rel)
            .parent()
            .unwrap_or(Path::new(""))
            .to_path_buf();
        for target in relative_links(&read(&rel)) {
            let path = root().join(&dir).join(&target);
            assert!(
                path.exists(),
                "{rel}: link target `{target}` does not exist"
            );
            checked += 1;
        }
    }
    assert!(
        checked >= 10,
        "expected the docs to link to each other; only {checked} relative links found"
    );
}
