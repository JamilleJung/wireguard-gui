//! Pure layout and navigation rules shared with `ui/*.slint`. The Slint files
//! implement the same numbers; the UI tests check the rendered result and
//! these unit tests pin the rules themselves.
// The size constants and rules mirror the Slint files; outside tests only
// the peer paging and filtering are called from Rust.
#![cfg_attr(not(test), allow(dead_code))]

/// Main window minimum size (logical px).
pub const MAIN_MIN: (f32, f32) = (720.0, 480.0);
/// Editor window minimum size (logical px).
pub const EDITOR_MIN: (f32, f32) = (520.0, 520.0);
/// Below this main-window width the tunnel list narrows.
pub const NARROW_BREAKPOINT: f32 = 860.0;
pub const LIST_WIDTH_WIDE: f32 = 270.0;
pub const LIST_WIDTH_NARROW: f32 = 240.0;
/// Peers shown per page in the detail pane.
pub const PEERS_PER_PAGE: usize = 5;

/// Width of the tunnel list for a given window width.
pub fn list_width(window_width: f32) -> f32 {
    if window_width < NARROW_BREAKPOINT {
        LIST_WIDTH_NARROW
    } else {
        LIST_WIDTH_WIDE
    }
}

/// New scroll offset (`viewport-y`, zero or negative) that keeps an item at
/// `item_y..item_y + item_h` (content coordinates) inside the visible area.
pub fn ensure_visible(viewport_y: f32, visible_h: f32, item_y: f32, item_h: f32) -> f32 {
    let top = -viewport_y;
    if item_y < top || item_h > visible_h {
        -item_y
    } else if item_y + item_h > top + visible_h {
        -(item_y + item_h - visible_h).max(0.0)
    } else {
        viewport_y
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PeerPage {
    pub start: usize,
    pub end: usize,
    pub page: usize,
    pub pages: usize,
}

impl PeerPage {
    pub fn label(&self, total: usize, matched: usize) -> String {
        if matched == 0 {
            return if total == 0 {
                String::new()
            } else {
                format!("No peers match (of {total})")
            };
        }
        let filtered = if matched < total {
            format!(" (filtered from {total})")
        } else {
            String::new()
        };
        if self.pages <= 1 {
            format!(
                "{matched} peer{}{filtered}",
                if matched == 1 { "" } else { "s" }
            )
        } else {
            format!(
                "Peers {}–{} of {matched}{filtered}",
                self.start + 1,
                self.end
            )
        }
    }
}

/// The slice of `total` items shown on (clamped) page `page`.
pub fn peer_page(total: usize, page: usize, per_page: usize) -> PeerPage {
    let per_page = per_page.max(1);
    let pages = total.div_ceil(per_page).max(1);
    let page = page.min(pages - 1);
    let start = (page * per_page).min(total);
    PeerPage {
        start,
        end: (start + per_page).min(total),
        page,
        pages,
    }
}

/// Indices of peers matching a case-insensitive filter on key, endpoint or
/// allowed IPs.
pub fn filter_peers<'a>(peers: impl IntoIterator<Item = [&'a str; 3]>, query: &str) -> Vec<usize> {
    let q = query.trim().to_ascii_lowercase();
    peers
        .into_iter()
        .enumerate()
        .filter(|(_, fields)| {
            q.is_empty() || fields.iter().any(|f| f.to_ascii_lowercase().contains(&q))
        })
        .map(|(i, _)| i)
        .collect()
}

/// Keyboard shortcuts of the main window: (keys as shown in Help, action).
/// docs/help.md lists every entry; the UI tests dispatch each one.
pub const SHORTCUTS: &[(&str, &str)] = &[
    ("Ctrl+1 / Ctrl+2 / Ctrl+3", "Tunnels / Log / Backup tab"),
    ("Ctrl+O", "Import tunnel(s) from file"),
    ("Ctrl+I", "Import from QR code"),
    ("Ctrl+N", "Create tunnel"),
    ("Ctrl+E", "Edit the selected tunnel"),
    ("Ctrl+T", "Activate / deactivate the selected tunnel"),
    ("Ctrl+D", "Diagnose the selected tunnel"),
    ("Ctrl+R / F5", "Refresh the current tab"),
    (
        "Ctrl+S",
        "Export all tunnels (Tunnels tab) / save the log (Log tab)",
    ),
    ("F1", "Help"),
    ("Ctrl+U", "Check for updates"),
    ("Tab / Shift+Tab", "Move between controls"),
    ("Enter / Space", "Press the focused button or toggle"),
    ("Up / Down", "Previous / next tunnel in the list"),
    (
        "PageUp / PageDown",
        "Scroll the details, Help, log and changelog",
    ),
    ("Escape", "Close a dialog window or the editor"),
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tunnel_list_narrows_below_the_breakpoint() {
        assert_eq!(list_width(MAIN_MIN.0), LIST_WIDTH_NARROW);
        assert_eq!(list_width(NARROW_BREAKPOINT - 1.0), LIST_WIDTH_NARROW);
        assert_eq!(list_width(NARROW_BREAKPOINT), LIST_WIDTH_WIDE);
        // At the minimum width the detail pane keeps enough room for the
        // narrowed label column plus the action buttons.
        assert!(MAIN_MIN.0 - list_width(MAIN_MIN.0) >= 480.0);
    }

    #[test]
    fn focused_items_are_scrolled_into_view() {
        // Already visible: unchanged.
        assert_eq!(ensure_visible(0.0, 300.0, 100.0, 30.0), 0.0);
        // Below the fold: scroll just enough.
        assert_eq!(ensure_visible(0.0, 300.0, 400.0, 30.0), -130.0);
        // Above the top: scroll up to it.
        assert_eq!(ensure_visible(-200.0, 300.0, 50.0, 30.0), -50.0);
        // Taller than the viewport: align its top.
        assert_eq!(ensure_visible(0.0, 20.0, 10.0, 30.0), -10.0);
    }

    #[test]
    fn peer_pages_are_clamped_and_labelled() {
        let p = peer_page(12, 0, 5);
        assert_eq!((p.start, p.end, p.pages), (0, 5, 3));
        assert_eq!(p.label(12, 12), "Peers 1–5 of 12");
        let p = peer_page(12, 9, 5);
        assert_eq!((p.start, p.end, p.page), (10, 12, 2));
        let p = peer_page(0, 3, 5);
        assert_eq!((p.start, p.end, p.page, p.pages), (0, 0, 0, 1));
        assert_eq!(p.label(0, 0), "");
        assert_eq!(peer_page(1, 0, 5).label(1, 1), "1 peer");
        assert_eq!(peer_page(2, 0, 5).label(9, 2), "2 peers (filtered from 9)");
        assert_eq!(peer_page(0, 0, 5).label(9, 0), "No peers match (of 9)");
    }

    #[test]
    fn peer_filter_matches_key_endpoint_or_allowed_ips() {
        let peers = [
            ["AAA=", "vpn.example.com:51820", "10.0.0.0/24"],
            ["BBB=", "", "192.168.1.0/24"],
        ];
        assert_eq!(filter_peers(peers, ""), vec![0, 1]);
        assert_eq!(filter_peers(peers, "EXAMPLE"), vec![0]);
        assert_eq!(filter_peers(peers, "192.168"), vec![1]);
        assert_eq!(filter_peers(peers, "bbb"), vec![1]);
        assert!(filter_peers(peers, "nothing").is_empty());
    }

    #[test]
    fn help_documents_every_shortcut() {
        let help = include_str!("../docs/help.md");
        for (keys, _) in SHORTCUTS {
            assert!(
                help.contains(keys),
                "docs/help.md is missing shortcut {keys}"
            );
        }
    }
}
