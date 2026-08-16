//! The central color palette.
//!
//! `Theme` is an immutable `Copy` value owned by [`crate::ui::app::App`] and
//! threaded into every draw call. It deliberately is *not* a mutable global:
//! the sibling Go project this app is being brought up to parity with keeps
//! its palette as package-level `var`s reassigned on toggle, with an explicit
//! comment warning that mutating them from anywhere but the render loop is a
//! data race. A `Copy` struct on `App` makes that whole class of bug
//! impossible to introduce by construction.

use ratatui::style::Color;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThemeMode {
    Dark,
    Light,
}

#[derive(Debug, Clone, Copy)]
pub struct Theme {
    pub mode: ThemeMode,

    pub fg: Color,
    pub dim: Color,
    pub accent: Color,
    pub ok: Color,
    pub warn: Color,
    pub error: Color,
    pub info: Color,

    /// Used once selection highlighting grows beyond the current
    /// `Modifier::REVERSED` (Step 7).
    pub sel_fg: Color,
    pub sel_bg: Color,

    /// Used once the tab bar lands (Step 4).
    pub tab_active_fg: Color,
    pub tab_active_bg: Color,
    pub tab_inactive: Color,

    /// Used once hold/essential badges and the selection-count badge land
    /// (Steps 5-6).
    pub badge_hold: Color,
    pub badge_essential: Color,
    pub badge_selected: Color,

    pub elevated_fg: Color,
    pub elevated_bg: Color,
    pub readonly_fg: Color,
    pub readonly_bg: Color,

    pub border: Color,
    pub title: Color,
}

impl Theme {
    pub fn dark() -> Self {
        Self {
            mode: ThemeMode::Dark,
            fg: Color::Reset,
            dim: Color::DarkGray,
            accent: Color::Cyan,
            ok: Color::Green,
            warn: Color::Yellow,
            error: Color::Red,
            info: Color::Cyan,
            sel_fg: Color::Black,
            sel_bg: Color::Cyan,
            tab_active_fg: Color::Black,
            tab_active_bg: Color::Cyan,
            tab_inactive: Color::DarkGray,
            badge_hold: Color::Yellow,
            badge_essential: Color::Magenta,
            badge_selected: Color::Green,
            elevated_fg: Color::Black,
            elevated_bg: Color::Yellow,
            readonly_fg: Color::Black,
            readonly_bg: Color::Cyan,
            border: Color::Yellow,
            title: Color::Reset,
        }
    }

    pub fn light() -> Self {
        Self {
            mode: ThemeMode::Light,
            fg: Color::Black,
            dim: Color::Gray,
            // Cyan and Yellow are the two hues that read poorly on a white
            // background; everything else is shared with the dark palette.
            accent: Color::Blue,
            ok: Color::Green,
            warn: Color::Rgb(150, 100, 0),
            error: Color::Red,
            info: Color::Blue,
            sel_fg: Color::White,
            sel_bg: Color::Blue,
            tab_active_fg: Color::White,
            tab_active_bg: Color::Blue,
            tab_inactive: Color::Gray,
            badge_hold: Color::Rgb(150, 100, 0),
            badge_essential: Color::Magenta,
            badge_selected: Color::Green,
            elevated_fg: Color::White,
            elevated_bg: Color::Rgb(150, 100, 0),
            readonly_fg: Color::White,
            readonly_bg: Color::Blue,
            border: Color::Rgb(150, 100, 0),
            title: Color::Black,
        }
    }

    pub fn for_mode(mode: ThemeMode) -> Self {
        match mode {
            ThemeMode::Dark => Self::dark(),
            ThemeMode::Light => Self::light(),
        }
    }

    pub fn toggled(self) -> Self {
        match self.mode {
            ThemeMode::Dark => Self::light(),
            ThemeMode::Light => Self::dark(),
        }
    }
}

/// Tier 1 of theme detection: `APTNAV_THEME=light|dark`. Any other value
/// (including `auto`, unset, or unrecognized) returns `None` and detection
/// falls through to the next tier; only a recognized value here should make
/// the caller set `theme_forced`.
pub fn mode_from_env() -> Option<ThemeMode> {
    match std::env::var("APTNAV_THEME").ok()?.trim().to_ascii_lowercase().as_str() {
        "dark" => Some(ThemeMode::Dark),
        "light" => Some(ThemeMode::Light),
        _ => None,
    }
}

/// Tier 2: `$COLORFGBG`, e.g. `"15;0"` (light-on-dark) or `"0;15"`
/// (dark-on-light) — set by some terminals/multiplexers with no terminal
/// round-trip required, unlike OSC 11. The background is whichever field
/// comes last (some terminals emit a middle "default background" token).
/// Palette indices 0-6 and 8 are the dark ANSI colors; treat anything else
/// (7, 9-15, or a 256-color index) as light.
pub fn mode_from_colorfgbg(v: &str) -> Option<ThemeMode> {
    let bg = v.rsplit(';').next()?.trim();
    let idx: u8 = bg.parse().ok()?;
    Some(if matches!(idx, 0..=6 | 8) {
        ThemeMode::Dark
    } else {
        ThemeMode::Light
    })
}

/// Resolve theme mode from the environment alone (Tiers 1-2; Tier 3, the
/// OSC 11 terminal query, is a separate opt-in step since it touches the
/// tty). Returns the mode plus whether it should be treated as forced
/// (suppressing any later terminal-query result).
pub fn resolve_from_env() -> Option<(ThemeMode, bool)> {
    if let Some(m) = mode_from_env() {
        return Some((m, true));
    }
    if let Some(m) = std::env::var("COLORFGBG")
        .ok()
        .and_then(|v| mode_from_colorfgbg(&v))
    {
        return Some((m, false));
    }
    None
}

/// Relative luminance (Rec. 709) of a 16-bit-per-channel color, normalized to
/// `0.0..=1.0`. Below `0.5` reads as a dark background.
pub fn luminance(r: u16, g: u16, b: u16) -> f32 {
    let rf = r as f32 / 65535.0;
    let gf = g as f32 / 65535.0;
    let bf = b as f32 / 65535.0;
    0.2126 * rf + 0.7152 * gf + 0.0722 * bf
}

/// Parse an OSC 11 background-color reply, e.g.
/// `\x1b]11;rgb:0000/0000/0000\x07` (BEL-terminated) or
/// `\x1b]11;rgb:ffff/ffff/ffff\x1b\\` (ST-terminated). Terminals vary in how
/// many hex digits they send per channel (1-4); each is scaled to 16 bits
/// before computing luminance so a 2-digit and a 4-digit reply for "black"
/// agree. Any deviation from that shape — missing `rgb:`, wrong component
/// count, non-hex digits — returns `None` rather than guessing.
pub fn parse_osc11(reply: &str) -> Option<ThemeMode> {
    let start = reply.find("rgb:")? + 4;
    let rest = &reply[start..];
    let end = rest.find(['\u{07}', '\u{1b}']).unwrap_or(rest.len());
    let body = &rest[..end];

    let mut parts = body.splitn(4, '/');
    let r = scale_hex_to_u16(parts.next()?)?;
    let g = scale_hex_to_u16(parts.next()?)?;
    let b = scale_hex_to_u16(parts.next()?)?;
    if parts.next().is_some() {
        return None; // a 4th component means this wasn't rgb:R/G/B after all
    }

    Some(if luminance(r, g, b) < 0.5 {
        ThemeMode::Dark
    } else {
        ThemeMode::Light
    })
}

fn scale_hex_to_u16(hex: &str) -> Option<u16> {
    if hex.is_empty() || hex.len() > 4 || !hex.bytes().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    let value = u32::from_str_radix(hex, 16).ok()?;
    let max = 16u32.pow(hex.len() as u32) - 1;
    Some(((value as u64 * 65535) / max as u64) as u16)
}

/// Tier 3: ask the terminal directly via OSC 11 and read its reply from the
/// tty, bounded by `timeout`. Must be called after `enable_raw_mode()` (so
/// the reply isn't echoed to the screen and arrives byte-by-byte rather than
/// line-buffered) and *before* `EnterAlternateScreen` (so the query and its
/// reply land on the normal screen, not the alternate one the rest of the
/// app draws into). Any failure — unsupported terminal, timeout, garbage —
/// returns `None`; the caller falls back to whatever Tier 1/2 already chose,
/// or the default.
pub fn query_terminal(timeout: std::time::Duration) -> Option<ThemeMode> {
    use std::io::{Read, Write};

    std::io::stdout().write_all(b"\x1b]11;?\x07").ok()?;
    std::io::stdout().flush().ok()?;

    let deadline = std::time::Instant::now() + timeout;
    let mut buf: Vec<u8> = Vec::with_capacity(64);
    let mut chunk = [0u8; 64];
    let mut stdin = std::io::stdin();

    loop {
        let remaining = deadline.checked_duration_since(std::time::Instant::now())?;
        if !poll_stdin_readable(remaining) {
            return None;
        }
        let n = stdin.read(&mut chunk).ok()?;
        if n == 0 {
            return None;
        }
        buf.extend_from_slice(&chunk[..n]);
        if buf.contains(&0x07) || buf.windows(2).any(|w| w == [0x1b, b'\\']) {
            break;
        }
        if buf.len() > 128 {
            return None; // not a terminator we recognize; stop reading garbage
        }
    }

    parse_osc11(std::str::from_utf8(&buf).ok()?)
}

fn poll_stdin_readable(timeout: std::time::Duration) -> bool {
    let mut pfd = libc::pollfd {
        fd: 0, // stdin
        events: libc::POLLIN,
        revents: 0,
    };
    let ms = timeout.as_millis().min(i32::MAX as u128) as i32;
    // Safety: `pfd` is a single, stack-local, properly initialized pollfd;
    // `poll` only reads/writes through the pointer we pass for the duration
    // of the call.
    let ret = unsafe { libc::poll(&mut pfd, 1, ms) };
    ret > 0 && (pfd.revents & libc::POLLIN) != 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn colorfgbg_dark_background() {
        assert_eq!(mode_from_colorfgbg("15;0"), Some(ThemeMode::Dark));
    }

    #[test]
    fn colorfgbg_light_background() {
        assert_eq!(mode_from_colorfgbg("0;15"), Some(ThemeMode::Light));
    }

    #[test]
    fn colorfgbg_non_numeric_background_is_none() {
        assert_eq!(mode_from_colorfgbg("0;default"), None);
    }

    #[test]
    fn colorfgbg_missing_semicolon_reads_whole_value_as_background() {
        // A single field with no `;` is still parsed as the (only) field,
        // which `rsplit(';').next()` returns unchanged.
        assert_eq!(mode_from_colorfgbg("0"), Some(ThemeMode::Dark));
    }

    #[test]
    fn toggled_round_trips() {
        let dark = Theme::dark();
        assert_eq!(dark.toggled().mode, ThemeMode::Light);
        assert_eq!(dark.toggled().toggled().mode, ThemeMode::Dark);
    }

    #[test]
    fn luminance_boundary_at_one_half() {
        assert!(luminance(32767, 32767, 32767) < 0.5);
        assert!(luminance(32768, 32768, 32768) >= 0.5);
        assert_eq!(luminance(0, 0, 0), 0.0);
        assert_eq!(luminance(65535, 65535, 65535), 1.0);
    }

    #[test]
    fn osc11_bel_terminated_black_is_dark() {
        assert_eq!(
            parse_osc11("\x1b]11;rgb:0000/0000/0000\x07"),
            Some(ThemeMode::Dark)
        );
    }

    #[test]
    fn osc11_st_terminated_white_is_light() {
        assert_eq!(
            parse_osc11("\x1b]11;rgb:ffff/ffff/ffff\x1b\\"),
            Some(ThemeMode::Light)
        );
    }

    #[test]
    fn osc11_eight_bit_form() {
        assert_eq!(
            parse_osc11("\x1b]11;rgb:00/00/00\x07"),
            Some(ThemeMode::Dark)
        );
        assert_eq!(
            parse_osc11("\x1b]11;rgb:ff/ff/ff\x07"),
            Some(ThemeMode::Light)
        );
    }

    #[test]
    fn osc11_twelve_bit_form() {
        assert_eq!(
            parse_osc11("\x1b]11;rgb:000/000/000\x07"),
            Some(ThemeMode::Dark)
        );
        assert_eq!(
            parse_osc11("\x1b]11;rgb:fff/fff/fff\x1b\\"),
            Some(ThemeMode::Light)
        );
    }

    #[test]
    fn osc11_truncated_or_garbage_is_none() {
        assert_eq!(parse_osc11("\x1b]11;rgb:0000/0000\x07"), None); // missing component
        assert_eq!(parse_osc11("\x1b]11;rgb:zzzz/zzzz/zzzz\x07"), None); // non-hex
        assert_eq!(parse_osc11("garbage, no rgb: prefix at all"), None);
        assert_eq!(parse_osc11(""), None);
    }
}
