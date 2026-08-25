//! Contract tests for the compiled-in Command Center assets.
//!
//! These guard the theming contract rather than the layout:
//!
//! - `theme.css` defines the palette twice for the light theme (once for the
//!   system preference, once for the explicit `[data-theme="light"]` opt-in)
//!   because CSS cannot share a declaration block across a media-query
//!   boundary. Nothing but a test stops those two lists from drifting apart,
//!   and a drift shows up as one stray dark-theme colour on a light page.
//! - Every page the server serves must read its colours from `theme.css`.
//!   A hardcoded hex in a consumer stylesheet is invisible in whichever theme
//!   the author happened to be looking at.

use super::command_center_assets::{
    APP_JS, BLAST_RADIUS_HTML, CALL_CHAIN_HTML, COUPLING_HTML, INDEX_HTML, STYLES_CSS, THEME_CSS,
};

/// Strip `/* … */` comments so documentation prose can mention colours
/// without tripping the hardcoded-colour scan.
fn strip_comments(src: &str) -> String {
    let mut out = String::with_capacity(src.len());
    let mut rest = src;
    while let Some(start) = rest.find("/*") {
        out.push_str(&rest[..start]);
        match rest[start + 2..].find("*/") {
            Some(end) => rest = &rest[start + 2 + end + 2..],
            None => return out,
        }
    }
    out.push_str(rest);
    out
}

/// Collect the custom-property names declared inside the block that follows
/// `selector`, up to its closing brace at column 0.
fn tokens_declared_in(css: &str, selector: &str) -> Vec<String> {
    let start = css
        .find(selector)
        .unwrap_or_else(|| panic!("theme.css is missing the `{selector}` block"));
    let body = &css[start..];
    let open = body.find('{').expect("selector block has no opening brace");
    let end = body.find("\n}").expect("selector block has no closing brace");
    let mut names: Vec<String> = body[open..end]
        .lines()
        .filter_map(|line| {
            let line = line.trim();
            let name = line.strip_prefix("--")?;
            let colon = name.find(':')?;
            Some(format!("--{}", &name[..colon]))
        })
        .collect();
    names.sort();
    names.dedup();
    names
}

/// The two light-theme declaration blocks must declare exactly the same
/// tokens. If they drift, whichever token is missing silently falls back to
/// its dark value on a light page.
#[test]
fn light_theme_blocks_declare_the_same_tokens() {
    let css = String::from_utf8(THEME_CSS.to_vec()).expect("theme.css is not utf-8");
    let css = strip_comments(&css);

    let via_media = tokens_declared_in(&css, ":root:not([data-theme=\"dark\"])");
    let via_attr = tokens_declared_in(&css, ":root[data-theme=\"light\"]");

    assert!(
        !via_media.is_empty(),
        "the prefers-color-scheme light block declared no tokens"
    );
    assert_eq!(
        via_media, via_attr,
        "the two light-theme blocks in theme.css have drifted; keep them in sync"
    );
}

/// Every token the light theme overrides must exist in the dark base, and
/// vice versa (bar `--font-mono`, which is deliberately theme-invariant and
/// declared only once).
#[test]
fn light_theme_covers_every_dark_token() {
    let css = String::from_utf8(THEME_CSS.to_vec()).expect("theme.css is not utf-8");
    let css = strip_comments(&css);

    let dark: Vec<String> = tokens_declared_in(&css, ":root {")
        .into_iter()
        .filter(|t| t != "--font-mono")
        .collect();
    let light = tokens_declared_in(&css, ":root[data-theme=\"light\"]");

    assert_eq!(
        dark, light,
        "dark and light palettes declare different tokens; every colour must be \
         overridden in both themes"
    );
}

/// No consumer may hardcode a colour — they all read tokens from `theme.css`.
/// `theme.css` itself is exempt: it is where the literals live.
#[test]
fn consumers_hardcode_no_colours() {
    let consumers: [(&str, &[u8]); 3] = [
        ("styles.css", STYLES_CSS),
        ("index.html", INDEX_HTML),
        ("app.js", APP_JS),
    ];
    let mut offenders = Vec::new();

    for (name, bytes) in consumers {
        let src = String::from_utf8(bytes.to_vec()).expect("asset is not utf-8");
        for (n, line) in strip_comments(&src).lines().enumerate() {
            if let Some(hex) = find_colour_literal(line) {
                offenders.push(format!("{name}:{}: {hex}", n + 1));
            }
        }
    }

    for (name, src) in [
        ("blast-radius.html", BLAST_RADIUS_HTML),
        ("call-chain.html", CALL_CHAIN_HTML),
        ("coupling.html", COUPLING_HTML),
    ] {
        for (n, line) in strip_comments(src).lines().enumerate() {
            if let Some(hex) = find_colour_literal(line) {
                offenders.push(format!("{name}:{}: {hex}", n + 1));
            }
        }
    }

    assert!(
        offenders.is_empty(),
        "hardcoded colours found — use a token from theme.css instead:\n  {}",
        offenders.join("\n  ")
    );
}

/// Return the first `#rgb` / `#rrggbb` literal on the line, if any. The
/// scanline gradient in `theme.css` is the only intentional literal and lives
/// outside the consumer set.
fn find_colour_literal(line: &str) -> Option<String> {
    let bytes = line.as_bytes();
    let mut i = 0;
    while let Some(off) = line[i..].find('#') {
        let at = i + off;
        let digits: String = bytes[at + 1..]
            .iter()
            .take_while(|b| b.is_ascii_hexdigit())
            .map(|&b| b as char)
            .collect();
        // Only 3/6/8-digit runs that end cleanly are colour literals; this
        // keeps `#graph`, `#tab-overview` and other id selectors out.
        let ends_cleanly = bytes
            .get(at + 1 + digits.len())
            .is_none_or(|b| !b.is_ascii_alphanumeric() && *b != b'-' && *b != b'_');
        if matches!(digits.len(), 3 | 6 | 8) && ends_cleanly {
            return Some(format!("#{digits}"));
        }
        i = at + 1;
    }
    None
}

/// The SPA shell must load the shared palette before its own stylesheet, and
/// must set the stored theme before first paint so the page never flashes the
/// wrong palette.
#[test]
fn index_html_wires_the_theme() {
    let html = String::from_utf8(INDEX_HTML.to_vec()).expect("index.html is not utf-8");

    let theme_at = html
        .find("/theme.css")
        .expect("index.html does not load /theme.css");
    let styles_at = html
        .find("/styles.css")
        .expect("index.html does not load /styles.css");
    assert!(
        theme_at < styles_at,
        "theme.css must be loaded before styles.css so the tokens are defined first"
    );

    // The script only has to run before the body paints, so its position
    // relative to the stylesheet links does not matter — being inside <head>
    // does.
    let head_end = html.find("</head>").expect("index.html has no </head>");
    assert!(
        html.find("lain-theme").is_some_and(|at| at < head_end),
        "the pre-paint theme script must run in <head>, before the page renders"
    );
    assert!(
        html.contains("crt-scanlines"),
        "index.html should opt into the shared scanline overlay"
    );
}

/// Each standalone detail view is reached by a direct link from an agent, so
/// each has to load the palette and honour the stored theme on its own.
#[test]
fn detail_views_load_the_shared_palette() {
    for (name, src) in [
        ("blast-radius.html", BLAST_RADIUS_HTML),
        ("call-chain.html", CALL_CHAIN_HTML),
        ("coupling.html", COUPLING_HTML),
    ] {
        assert!(
            src.contains("/theme.css"),
            "{name} does not load the shared palette"
        );
        assert!(
            src.contains("lain-theme"),
            "{name} does not honour the stored theme choice"
        );
        assert!(
            src.contains("crt-scanlines"),
            "{name} does not opt into the scanline overlay"
        );
    }
}
