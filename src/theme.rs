//! Display themes: the default palette, a high-contrast palette, and a
//! plain-text (ASCII) fallback for symbol glyphs. Chosen with `--theme NAME`
//! or `WG_GUI_THEME=NAME`; the command line wins.

use std::sync::atomic::{AtomicBool, Ordering};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ThemeChoice {
    pub high_contrast: bool,
    /// Replace symbols/emoji glyphs with ASCII labels.
    pub ascii: bool,
}

/// Accepted names, documented in docs/help.md and ACCESSIBILITY.md. Options
/// combine with `+` or `,`, e.g. `high-contrast+ascii`.
pub const THEME_NAMES: &[&str] = &["default", "high-contrast", "ascii"];

pub fn parse(spec: &str) -> Result<ThemeChoice, String> {
    let mut choice = ThemeChoice::default();
    for part in spec
        .split(['+', ','])
        .map(str::trim)
        .filter(|p| !p.is_empty())
    {
        match part.to_ascii_lowercase().as_str() {
            "default" | "normal" => {}
            "high-contrast" | "highcontrast" | "contrast" | "hc" => choice.high_contrast = true,
            "ascii" | "plain" | "plain-text" | "text" => choice.ascii = true,
            other => {
                return Err(format!(
                    "unknown theme '{other}' (use {})",
                    THEME_NAMES.join(", ")
                ));
            }
        }
    }
    Ok(choice)
}

/// Resolve the theme from the command-line value (if any) and the
/// environment. An invalid environment value falls back to the default.
pub fn resolve(cli: Option<&str>, env: Option<&str>) -> Result<ThemeChoice, String> {
    match cli {
        Some(spec) => parse(spec),
        None => Ok(env.and_then(|spec| parse(spec).ok()).unwrap_or_default()),
    }
}

static ASCII: AtomicBool = AtomicBool::new(false);

pub fn set_ascii(on: bool) {
    ASCII.store(on, Ordering::Relaxed);
}

pub fn ascii() -> bool {
    ASCII.load(Ordering::Relaxed)
}

/// Map the symbols this app uses to ASCII when the plain-text theme is on.
pub fn text(s: &str) -> String {
    if ascii() { to_ascii(s) } else { s.to_string() }
}

pub fn to_ascii(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '…' => out.push_str("..."),
            '→' => out.push_str("->"),
            '←' => out.push_str("<-"),
            '↓' => out.push_str("down"),
            '↑' => out.push_str("up"),
            '—' | '–' => out.push('-'),
            '“' | '”' => out.push('"'),
            '‘' | '’' => out.push('\''),
            '•' => out.push('*'),
            '◦' => out.push('-'),
            '·' => out.push('|'),
            '⚠' => out.push_str("Warning:"),
            '⧉' => out.push_str("[copy]"),
            '▾' | '▸' => out.push('+'),
            '▴' => out.push('-'),
            '✕' | '×' => out.push('x'),
            '✓' => out.push_str("OK"),
            '\u{fe0f}' => {}
            c => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_names_aliases_and_combinations() {
        assert_eq!(parse("default").unwrap(), ThemeChoice::default());
        assert!(parse("high-contrast").unwrap().high_contrast);
        assert!(parse("HC").unwrap().high_contrast);
        let both = parse("high-contrast+ascii").unwrap();
        assert!(both.high_contrast && both.ascii);
        assert_eq!(parse("contrast,plain").unwrap(), both);
        assert!(parse("neon").unwrap_err().contains("unknown theme"));
        for name in THEME_NAMES {
            assert!(parse(name).is_ok(), "{name}");
        }
    }

    #[test]
    fn command_line_overrides_environment() {
        assert!(resolve(Some("ascii"), Some("high-contrast")).unwrap().ascii);
        assert!(
            !resolve(Some("ascii"), Some("high-contrast"))
                .unwrap()
                .high_contrast
        );
        assert!(resolve(None, Some("high-contrast")).unwrap().high_contrast);
        assert_eq!(
            resolve(None, Some("bogus")).unwrap(),
            ThemeChoice::default()
        );
        assert!(resolve(Some("bogus"), None).is_err());
    }

    #[test]
    fn ascii_fallback_replaces_every_symbol_the_app_uses() {
        let s = to_ascii("Renamed a → b — ok… “x” ↓ 1 KiB/s ↑ ⚠ ⧉ ▾ ▴ • ◦ ✕");
        assert!(s.is_ascii(), "{s}");
        assert_eq!(to_ascii("Saving tunnel…"), "Saving tunnel...");
        assert_eq!(to_ascii("plain"), "plain");
    }
}
