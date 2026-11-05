//! Lightweight markdown rendering for the Help and changelog views.

use slint::{ModelRc, VecModel};

use crate::DocItem;

/// The in-app guide, embedded at build time. Slint can't render markdown, so the
/// Help window shows this raw text in a monospace view (still readable).
pub const HELP_TEXT: &str = include_str!("../../docs/help.md");

/// The changelog, embedded at build time and shown in the Updates window.
pub const CHANGELOG_TEXT: &str = include_str!("../../CHANGELOG.md");

/// Strip the inline markdown markers Slint can't render (`**bold**`, `` `code` ``,
/// `*italic*`, and `[label](url)` links) down to their visible text, so a Help /
/// changelog line reads cleanly. Hand-rolled (no regex crate): we drop `*` and
/// `` ` `` run markers and rewrite links to their label. This is intentionally
/// lenient — it's display-only text, not a parser, so unbalanced markers just
/// lose their stray symbol rather than failing.
fn strip_inline_md(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    while i < bytes.len() {
        let c = bytes[i];
        match c {
            // Drop emphasis/code markers (`*`, `**`, `` ` ``) — keep the text.
            b'*' | b'`' => {
                i += 1;
                // Collapse a doubled `**` into one skip.
                if i < bytes.len() && bytes[i] == c {
                    i += 1;
                }
            }
            // `[label](url)` -> `label`. Only rewrite a well-formed link; an
            // unmatched `[` is emitted verbatim.
            b'[' => {
                if let Some((label, consumed)) = parse_md_link(&s[i..]) {
                    out.push_str(&label);
                    i += consumed;
                } else {
                    out.push('[');
                    i += 1;
                }
            }
            _ => {
                // Copy this whole UTF-8 char (markers above are all ASCII, so
                // multi-byte chars only ever hit this arm).
                let ch_len = utf8_char_len(c);
                let end = (i + ch_len).min(bytes.len());
                out.push_str(&s[i..end]);
                i = end;
            }
        }
    }
    out
}

/// Byte length of a UTF-8 char from its leading byte.
fn utf8_char_len(b: u8) -> usize {
    if b < 0x80 {
        1
    } else if b >> 5 == 0b110 {
        2
    } else if b >> 4 == 0b1110 {
        3
    } else if b >> 3 == 0b11110 {
        4
    } else {
        1 // stray continuation byte — advance one to make progress
    }
}

/// Parse a `[label](url)` markdown link at the start of `s`. Returns the label
/// and the number of bytes consumed (the whole `[label](url)`), or None if `s`
/// doesn't start with a complete link.
fn parse_md_link(s: &str) -> Option<(String, usize)> {
    let bytes = s.as_bytes();
    if bytes.first() != Some(&b'[') {
        return None;
    }
    let close = s.find("](")?;
    let label = &s[1..close];
    // Labels don't span lines or contain a nested `[`.
    if label.contains('[') || label.contains('\n') {
        return None;
    }
    let after = &s[close + 2..];
    let paren = after.find(')')?;
    if after[..paren].contains('\n') {
        return None;
    }
    Some((label.to_string(), close + 2 + paren + 1))
}

/// Lightweight per-line markdown for the Help & changelog doc views: tag each
/// line with a `kind` MdRow styles. We don't render full markdown — just enough
/// (headings, bullets, blockquotes, fenced code, rules) to drop the literal
/// `#`/`**`/backtick noise. Inline markers are stripped for every kind except
/// code (which is shown verbatim).
pub fn md_items(src: &str) -> Vec<DocItem> {
    let mut items = Vec::new();
    let mut in_code = false;
    for raw in src.lines() {
        let trimmed = raw.trim();
        // Fenced code: ``` toggles the block; the fence lines themselves are
        // not emitted.
        if trimmed.starts_with("```") {
            in_code = !in_code;
            continue;
        }
        if in_code {
            // Verbatim — keep indentation, don't strip inline markers.
            items.push(DocItem {
                text: raw.into(),
                kind: "code".into(),
            });
            continue;
        }
        let (kind, text): (&str, String) =
            if trimmed == "---" || trimmed == "***" || trimmed == "___" {
                ("rule", String::new())
            } else if let Some(rest) = trimmed.strip_prefix("### ") {
                ("h3", strip_inline_md(rest))
            } else if let Some(rest) = trimmed.strip_prefix("## ") {
                ("h2", strip_inline_md(rest))
            } else if let Some(rest) = trimmed.strip_prefix("# ") {
                ("h1", strip_inline_md(rest))
            } else if let Some(rest) = trimmed.strip_prefix("> ") {
                ("quote", strip_inline_md(rest))
            } else if trimmed.is_empty() {
                ("blank", String::new())
            } else if let Some(rest) = bullet_rest(raw) {
                // Nested bullets (indented 2+ spaces) get a "  ◦ " hollow marker;
                // top-level ones a "• " filled marker.
                let nested = raw.len() - raw.trim_start().len() >= 2;
                let marker = if nested { "  ◦ " } else { "• " };
                ("bullet", format!("{marker}{}", strip_inline_md(rest)))
            } else {
                ("normal", strip_inline_md(trimmed))
            };
        items.push(DocItem {
            text: text.into(),
            kind: kind.into(),
        });
    }
    items
}

/// If `line` is a markdown bullet (`- ` or `* ` after optional indentation),
/// return the text after the marker. Avoids treating a `***` rule (already
/// handled) or a bare `*` as a bullet.
fn bullet_rest(line: &str) -> Option<&str> {
    let t = line.trim_start();
    t.strip_prefix("- ").or_else(|| t.strip_prefix("* "))
}

/// Build the `[DocItem]` model for a markdown doc view (Help / changelog).
pub fn md_items_model(src: &str) -> ModelRc<DocItem> {
    let items: Vec<DocItem> = md_items(src)
        .into_iter()
        .map(|item| DocItem {
            text: crate::theme::text(&item.text).into(),
            kind: item.kind,
        })
        .collect();
    ModelRc::new(VecModel::from(items))
}

/// Trim the dev-facing Keep-a-Changelog header/intro and the (usually empty)
/// `[Unreleased]` section for the in-app view — end users just want the released
/// versions, so start at the first `## [x.y.z]` heading.
pub fn changelog_for_display(src: &str) -> String {
    for (i, line) in src.lines().enumerate() {
        let t = line.trim_start();
        if t.starts_with("## [") && t.as_bytes().get(4).is_some_and(u8::is_ascii_digit) {
            return src.lines().skip(i).collect::<Vec<_>>().join("\n");
        }
    }
    src.to_string()
}

#[cfg(test)]
mod tests {
    use super::{md_items, strip_inline_md};

    /// Find the kind tag a given line text was assigned.
    fn kind_of<'a>(items: &'a [super::DocItem], text: &str) -> Option<&'a str> {
        items
            .iter()
            .find(|i| i.text == text)
            .map(|i| i.kind.as_str())
    }

    #[test]
    fn strips_bold_code_italic_markers() {
        assert_eq!(strip_inline_md("**bold** and `code`"), "bold and code");
        assert_eq!(strip_inline_md("an *italic* word"), "an italic word");
        // A standalone marker just vanishes (display-only, lenient).
        assert_eq!(strip_inline_md("a ** b"), "a  b");
    }

    #[test]
    fn rewrites_links_to_their_label() {
        assert_eq!(
            strip_inline_md("see [Keep a Changelog](https://example.com) here"),
            "see Keep a Changelog here"
        );
        // An unmatched bracket is left verbatim.
        assert_eq!(strip_inline_md("an [open bracket"), "an [open bracket");
    }

    #[test]
    fn preserves_non_ascii_text() {
        // Em dash and bullet glyphs must round-trip untouched.
        assert_eq!(strip_inline_md("a — b • c"), "a — b • c");
    }

    #[test]
    fn classifies_headings_rules_and_blanks() {
        let items = md_items("# Title\n## Sub\n### SubSub\n\n---\nplain line");
        assert_eq!(kind_of(&items, "Title"), Some("h1"));
        assert_eq!(kind_of(&items, "Sub"), Some("h2"));
        assert_eq!(kind_of(&items, "SubSub"), Some("h3"));
        // A blank and a rule both carry empty text; check by position/count.
        assert!(items.iter().any(|i| i.kind == "blank"));
        assert!(items.iter().any(|i| i.kind == "rule"));
        assert_eq!(kind_of(&items, "plain line"), Some("normal"));
    }

    #[test]
    fn bullets_get_markers_and_nesting() {
        let items = md_items("- top\n  - nested\n* star top");
        assert_eq!(kind_of(&items, "• top"), Some("bullet"));
        assert_eq!(kind_of(&items, "  ◦ nested"), Some("bullet"));
        assert_eq!(kind_of(&items, "• star top"), Some("bullet"));
    }

    #[test]
    fn quote_strips_marker() {
        let items = md_items("> a quoted **note**");
        assert_eq!(kind_of(&items, "a quoted note"), Some("quote"));
    }

    #[test]
    fn fenced_code_is_verbatim_and_skips_fences() {
        let items = md_items("```\nlet x = **not bold**;\n```\nafter");
        // The fence lines are dropped; the body keeps its markers verbatim.
        assert_eq!(kind_of(&items, "let x = **not bold**;"), Some("code"));
        assert!(!items.iter().any(|i| i.text.starts_with("```")));
        assert_eq!(kind_of(&items, "after"), Some("normal"));
    }

    #[test]
    fn real_docs_parse_without_panicking() {
        // Smoke: the embedded docs must parse and produce styled rows.
        let help = md_items(super::HELP_TEXT);
        let changelog = md_items(super::CHANGELOG_TEXT);
        assert!(help.iter().any(|i| i.kind == "h1"));
        assert!(changelog.iter().any(|i| i.kind == "h2"));
        // No emitted (non-code) line should still contain a literal `**`.
        assert!(
            !help
                .iter()
                .any(|i| i.kind != "code" && i.text.contains("**"))
        );
    }
}
