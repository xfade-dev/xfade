//! Ratatui rendering for the TUI. All layout decisions live here; the state
//! being rendered lives in `app.rs`. Verified with `TestBackend` snapshots.

use ratatui::{
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, Paragraph, Wrap},
    Frame,
};
use std::sync::OnceLock;
use xfade_core::store::db::StatsRow;
use xfade_core::ToolKind;

use crate::tui::app::{App, EditField, ProbeResult};

/// Stats window shown in the header (e.g. `7d`), parsed by `crate::parse_since`.
pub const STATS_DAYS: &str = "7d";

/// Minimum terminal size for the full layout. Below this the header tabs and
/// provider list lose too much to stay usable, so a "too small" notice is
/// shown instead of a broken list.
const MIN_WIDTH: u16 = 30;
const MIN_HEIGHT: u16 = 13;

pub fn draw(f: &mut Frame, app: &App, stats: &[StatsRow]) {
    let area = f.area();
    if area.width < MIN_WIDTH || area.height < MIN_HEIGHT {
        draw_too_small(f, area);
        return;
    }

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .margin(1)
        .constraints([
            Constraint::Length(5),
            Constraint::Min(3),
            Constraint::Length(3),
        ])
        .split(area);

    draw_fader(f, app, stats, chunks[0]);
    if app.is_editing() {
        draw_edit(f, app, chunks[1]);
    } else {
        draw_list(f, app, stats, chunks[1]);
    }
    draw_footer(f, app, chunks[2]);
}

/// Degraded layout below [`MIN_WIDTH`]×[`MIN_HEIGHT`]: name the requirement
/// instead of rendering a truncated list.
fn draw_too_small(f: &mut Frame, area: Rect) {
    let msg = format!(
        "terminal too small — need at least {MIN_WIDTH}×{MIN_HEIGHT} (now {}×{})",
        area.width, area.height
    );
    let paragraph = Paragraph::new(Span::styled(msg, Style::default().fg(Color::Yellow)))
        .alignment(Alignment::Center)
        .wrap(Wrap { trim: true });
    f.render_widget(paragraph, area);
}

/// Mini tab bar over all tools; the current one is inverted.
fn tool_tabs(app: &App) -> Line<'static> {
    let spans = ToolKind::ALL
        .iter()
        .flat_map(|t| {
            let style = if *t == app.tool {
                Style::default()
                    .fg(Color::Black)
                    .bg(Color::Cyan)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::DarkGray)
            };
            [
                Span::styled(format!(" {} ", t.as_str()), style),
                Span::raw(" "),
            ]
        })
        .collect::<Vec<_>>();
    Line::from(spans)
}

/// Cyan → magenta: the crossfade gradient endpoints shared by the wordmark and
/// the fader track. Used in truecolor mode only — see `GradientMode` for how
/// other terminals degrade.
const GRADIENT_FROM: (u8, u8, u8) = (0, 195, 255);
const GRADIENT_TO: (u8, u8, u8) = (255, 60, 190);

/// How the crossfade gradients render, decided once from the environment.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum GradientMode {
    /// `NO_COLOR` is set (no-color.org): no fg color — bold text, the knob
    /// glyph, and position carry the meaning.
    NoColor,
    /// No advertised truecolor: a two-tone ANSI cyan/magenta crossfade. The
    /// intro fade is truecolor-only, so it is skipped in this mode.
    Ansi,
    /// `COLORTERM=truecolor|24bit`: the full RGB gradient with intro fade.
    Truecolor,
}

/// Decide the gradient mode once per process. `COLORTERM` is the only widely
/// honored truecolor signal (`crossterm::style::available_color_count` only
/// detects 256-color via `TERM`, never truecolor).
fn gradient_mode() -> GradientMode {
    static MODE: OnceLock<GradientMode> = OnceLock::new();
    *MODE.get_or_init(|| {
        // no-color.org: present and non-empty suppresses color.
        let no_color = std::env::var_os("NO_COLOR").is_some_and(|v| !v.is_empty());
        let truecolor = std::env::var("COLORTERM")
            .is_ok_and(|v| v.eq_ignore_ascii_case("truecolor") || v.eq_ignore_ascii_case("24bit"));
        detect_gradient_mode(no_color, truecolor)
    })
}

fn detect_gradient_mode(no_color: bool, truecolor: bool) -> GradientMode {
    if no_color {
        GradientMode::NoColor
    } else if truecolor {
        GradientMode::Truecolor
    } else {
        GradientMode::Ansi
    }
}

/// Color at gradient position `t` (0 = cyan/local end, 1 = magenta/cloud
/// end), faded toward black by `progress` where the terminal supports it.
fn gradient_color(mode: GradientMode, t: f32, progress: f32) -> Color {
    match mode {
        GradientMode::NoColor => Color::Reset,
        GradientMode::Ansi => {
            if t < 0.5 {
                Color::Cyan
            } else {
                Color::Magenta
            }
        }
        GradientMode::Truecolor => {
            lerp_rgb(dim(GRADIENT_FROM, progress), dim(GRADIENT_TO, progress), t)
        }
    }
}

/// Interpolate between two RGB endpoints (t in 0..=1).
fn lerp_rgb(from: (u8, u8, u8), to: (u8, u8, u8), t: f32) -> Color {
    let t = t.clamp(0.0, 1.0);
    let mix = |a: u8, b: u8| (a as f32 + (b as f32 - a as f32) * t).round() as u8;
    Color::Rgb(mix(from.0, to.0), mix(from.1, to.1), mix(from.2, to.2))
}

/// Scale an RGB tuple toward black (0.0 = black, 1.0 = unchanged), used to
/// fade the header gradients in during the intro.
fn dim((r, g, b): (u8, u8, u8), f: f32) -> (u8, u8, u8) {
    let f = f.clamp(0.0, 1.0);
    (
        (r as f32 * f) as u8,
        (g as f32 * f) as u8,
        (b as f32 * f) as u8,
    )
}

/// Header title: a crossfade wordmark — `◀ xfade ▶` with `xfade` rendered as
/// a cyan→magenta gradient, one span per character, plus the current tool
/// dimmed after it.
fn title_line(tool: ToolKind, progress: f32) -> Line<'static> {
    let mut spans = vec![
        Span::raw(" "),
        Span::styled("◀ ", Style::default().fg(Color::Cyan)),
    ];
    let mode = gradient_mode();
    let word = "xfade";
    let n = word.chars().count();
    for (i, c) in word.chars().enumerate() {
        let t = if n <= 1 {
            0.0
        } else {
            i as f32 / (n - 1) as f32
        };
        spans.push(Span::styled(
            c.to_string(),
            Style::default()
                .fg(gradient_color(mode, t, progress))
                .add_modifier(Modifier::BOLD),
        ));
    }
    spans.push(Span::styled(" ▶", Style::default().fg(Color::Magenta)));
    spans.push(Span::styled(
        format!(" · {} ", tool.as_str()),
        Style::default().fg(Color::DarkGray),
    ));
    Line::from(spans)
}

/// The crossfader header: local ◀───[ ■ ]───▶ cloud, plus a status line with
/// route / latency / token usage for the active provider.
fn draw_fader(f: &mut Frame, app: &App, stats: &[StatsRow], area: Rect) {
    let active = app.active_provider();
    let flash = app.flash_until.is_some();
    let progress = app.intro_progress();
    let fader = fader_line(area.width.saturating_sub(2), app.fader_pos, flash, progress);

    let status = match active {
        Some(p) => {
            let route_style = if flash {
                Style::default()
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().add_modifier(Modifier::BOLD)
            };
            let mut spans = vec![
                Span::raw("Route: "),
                Span::styled(p.id.clone(), route_style),
                Span::raw("   Latency: "),
            ];
            if app.probing.contains(p.id.as_str()) {
                spans.push(Span::styled(
                    app.spinner_glyph().to_string(),
                    Style::default().fg(Color::Cyan),
                ));
            } else if let Some(result) = app.probes.get(&p.id) {
                match result {
                    ProbeResult::Ms(ms) => spans.push(Span::styled(
                        format!("{ms}ms"),
                        Style::default().fg(latency_color(*ms)),
                    )),
                    ProbeResult::Failed => {
                        spans.push(Span::styled("✖ timeout", Style::default().fg(Color::Red)))
                    }
                }
            } else if let Some(row) = stats.iter().find(|r| r.group == p.id) {
                spans.push(Span::styled(
                    format!("avg {}ms", row.avg_duration_ms),
                    Style::default().fg(latency_color(row.avg_duration_ms.max(0) as u128)),
                ));
            } else {
                spans.push(Span::styled("-", Style::default().fg(Color::DarkGray)));
            }
            let tokens = stats
                .iter()
                .find(|r| r.group == p.id)
                .map(|r| format_tokens((r.prompt_tokens + r.completion_tokens).max(0) as u64))
                .unwrap_or_else(|| "-".into());
            spans.push(Span::raw(format!("   Tokens({STATS_DAYS}): ")));
            spans.push(Span::raw(tokens));
            Line::from(spans)
        }
        None => Line::from("Route: (none)"),
    };

    let block = Block::default()
        .borders(Borders::ALL)
        .title(title_line(app.tool, progress));
    let text = Paragraph::new(vec![tool_tabs(app), fader, status]).block(block);
    f.render_widget(text, area);
}

/// The crossfader track as a cyan→magenta gradient, one span per cell.
fn gradient_track(cells: usize, progress: f32) -> Vec<Span<'static>> {
    let mode = gradient_mode();
    (0..cells)
        .map(|i| {
            let t = if cells <= 1 {
                0.0
            } else {
                i as f32 / (cells - 1) as f32
            };
            Span::styled("─", Style::default().fg(gradient_color(mode, t, progress)))
        })
        .collect()
}

/// The knob rides the gradient track in bold white; a switch flash inverts it.
fn knob_style(flash: bool) -> Style {
    let mut style = Style::default()
        .fg(Color::White)
        .add_modifier(Modifier::BOLD);
    if flash {
        style = style.add_modifier(Modifier::REVERSED);
    }
    style
}

/// `local ◀─…─■─…─▶ cloud` with the knob at a continuous `pos` (0.0 = local
/// end, 1.0 = cloud end). The track is a continuous cyan→magenta gradient and
/// the knob sits on top of it, so the whole line reads as a crossfade.
fn fader_line(width: u16, pos: f32, flash: bool, progress: f32) -> Line<'static> {
    let label_l = "local ";
    let label_r = " cloud";
    let track = (width as usize).saturating_sub(label_l.len() + label_r.len() + 2);
    let knob = ((pos.clamp(0.0, 1.0)) * track.saturating_sub(1) as f32).round() as usize;

    let mut spans = vec![
        Span::styled(label_l, Style::default().fg(Color::Cyan)),
        Span::raw("◀"),
    ];
    if track == 0 {
        // Keep the knob even when there is no room for a track.
        spans.push(Span::styled("■", knob_style(flash)));
    } else {
        for (i, cell) in gradient_track(track, progress).into_iter().enumerate() {
            if i == knob {
                spans.push(Span::styled("■", knob_style(flash)));
            } else {
                spans.push(cell);
            }
        }
    }
    spans.push(Span::raw("▶"));
    spans.push(Span::styled(label_r, Style::default().fg(Color::Magenta)));
    Line::from(spans)
}

/// Health color for a measured latency. Single source of truth for tiers —
/// both the status line and the list badges use it, so a given latency always
/// renders the same color everywhere.
fn latency_color(ms: u128) -> Color {
    if ms <= 100 {
        Color::Cyan
    } else if ms <= 300 {
        Color::Green
    } else if ms <= 800 {
        Color::Yellow
    } else {
        Color::Red
    }
}

/// Per-row latency badge: ⚡ fast / ● tiered by threshold / ✖ timeout.
fn latency_badge(result: &ProbeResult) -> (String, Color) {
    match *result {
        ProbeResult::Ms(ms) => (
            format!("{} {ms}ms", if ms <= 100 { "⚡" } else { "●" }),
            latency_color(ms),
        ),
        ProbeResult::Failed => ("✖ timeout".into(), Color::Red),
    }
}

fn draw_list(f: &mut Frame, app: &App, stats: &[StatsRow], area: Rect) {
    let items: Vec<ListItem> = app
        .providers
        .iter()
        .enumerate()
        .map(|(i, p)| {
            let mark = if p.is_active { "*" } else { " " };
            let base = p.base_url.as_deref().unwrap_or("(official)");
            let mut spans = vec![Span::raw(format!("{mark} {:<18} {base:<40} ", p.id))];
            if app.probing.contains(p.id.as_str()) {
                spans.push(Span::styled(
                    app.spinner_glyph().to_string(),
                    Style::default().fg(Color::Cyan),
                ));
            } else if let Some(result) = app.probes.get(&p.id) {
                let (badge, color) = latency_badge(result);
                spans.push(Span::styled(badge, Style::default().fg(color)));
            }
            if let Some(row) = stats.iter().find(|r| r.group == p.id && r.requests > 0) {
                let ok = 100.0 * (1.0 - row.errors as f64 / row.requests as f64);
                let color = if ok >= 95.0 {
                    Color::DarkGray
                } else {
                    Color::Red
                };
                spans.push(Span::styled(
                    format!("   {ok:.0}% ok"),
                    Style::default().fg(color),
                ));
            }
            let line = Line::from(spans);
            // Selection uses reverse video: the canonical, theme-independent
            // signal that survives monochrome and color-blindness (a fg-only
            // color does not). Active providers keep the `*` mark + green fg.
            let style = if i == app.selected {
                Style::default().add_modifier(Modifier::REVERSED)
            } else if p.is_active {
                Style::default().fg(Color::Green)
            } else {
                Style::default()
            };
            ListItem::new(line).style(style)
        })
        .collect();
    let list = List::new(items).block(
        Block::default()
            .borders(Borders::ALL)
            .title(format!(" Providers ({}) ", app.tool.as_str())),
    );
    f.render_widget(list, area);
}

fn draw_edit(f: &mut Frame, app: &App, area: Rect) {
    let Some(e) = app.edit.as_ref() else {
        return;
    };
    let field_line = |label: &str, value: &str, focused: bool, masked: bool| {
        let shown = if masked {
            if value.is_empty() {
                "(blank = keep current key)".to_string()
            } else {
                "*".repeat(value.len())
            }
        } else {
            value.to_string()
        };
        let style = if focused {
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default()
        };
        Line::from(vec![
            Span::raw(format!("{label:<10}")),
            Span::styled(shown, style),
            if focused {
                Span::styled("_", Style::default().fg(Color::Yellow))
            } else {
                Span::raw("")
            },
        ])
    };
    let lines = vec![
        field_line(
            "Base URL:",
            &e.base_url,
            e.field == EditField::BaseUrl,
            false,
        ),
        field_line("Model:", &e.model, e.field == EditField::Model, false),
        field_line("API Key:", &e.api_key, e.field == EditField::ApiKey, true),
        Line::from(""),
        Line::from(Span::styled(
            "blank Base URL = official login   [Tab] next field   [Enter] save   [Esc] cancel",
            Style::default().fg(Color::DarkGray),
        )),
    ];
    let block = Block::default()
        .borders(Borders::ALL)
        .title(format!(" Edit provider: {} ", e.provider_id));
    f.render_widget(Paragraph::new(lines).block(block), area);
}

fn draw_footer(f: &mut Frame, app: &App, area: Rect) {
    let hints = if app.is_editing() {
        "editing…"
    } else {
        "[↑/↓ j/k] move   [Enter] switch   [t] test   [e] edit   [Tab] tool   [q] quit"
    };
    let mut lines = vec![Line::from(Span::styled(
        hints,
        Style::default().fg(Color::DarkGray),
    ))];
    if let Some(m) = &app.message {
        lines.push(Line::from(m.clone()));
    }
    f.render_widget(Paragraph::new(lines), area);
}

/// Compact token count: 1234 -> "1.2k", 2_500_000 -> "2.5M".
fn format_tokens(n: u64) -> String {
    if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    } else if n >= 1_000 {
        format!("{:.1}k", n as f64 / 1_000.0)
    } else {
        n.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::app::{App, FADER_CLOUD, FADER_LOCAL, SPINNER};
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;
    use std::sync::Arc;
    use xfade_core::store::secrets::FileStore;
    use xfade_core::{Core, Provider, ToolKind};

    fn test_core(dir: &tempfile::TempDir) -> Core {
        let home = dir.path().join("home");
        let data = dir.path().join("data");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir_all(&data).unwrap();
        Core::with_paths(
            &home,
            &data,
            Arc::new(FileStore::new(data.join("secrets.json"))),
        )
        .unwrap()
    }

    fn buffer_text(t: &Terminal<TestBackend>) -> String {
        let b = t.backend().buffer().clone();
        b.content().iter().map(|c| c.symbol()).collect()
    }

    #[test]
    fn fader_header_shows_route_and_knob_on_cloud_side_for_remote() {
        let dir = tempfile::tempdir().unwrap();
        let core = test_core(&dir);
        core.add_provider(
            Provider::new(
                "glmcc",
                ToolKind::ClaudeCode,
                Some("http://gw.example:3000".into()),
            ),
            Some("sk-test"),
        )
        .unwrap();
        core.use_provider(ToolKind::ClaudeCode, "glmcc").unwrap();
        let app = App::new(ToolKind::ClaudeCode, &core);

        let backend = TestBackend::new(80, 20);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|f| draw(f, &app, &[])).unwrap();
        let text = buffer_text(&terminal);
        assert!(text.contains("local"), "fader label missing:\n{text}");
        assert!(text.contains("cloud"), "fader label missing:\n{text}");
        assert!(text.contains('■'), "fader knob missing:\n{text}");
        assert!(
            text.contains("Route: glmcc"),
            "status line missing:\n{text}"
        );
    }

    #[test]
    fn fader_knob_moves_toward_local_end_as_pos_decreases() {
        let local = fader_line(60, FADER_LOCAL, false, 1.0);
        let cloud = fader_line(60, FADER_CLOUD, false, 1.0);
        let pos = |l: &Line| {
            l.spans
                .iter()
                .map(|s| s.content.as_ref())
                .collect::<String>()
                .find('■')
                .unwrap()
        };
        assert!(pos(&local) < pos(&cloud));
        let mid = fader_line(60, 0.5, false, 1.0);
        assert!(pos(&local) < pos(&mid) && pos(&mid) < pos(&cloud));
    }

    #[test]
    fn fader_knob_is_white_and_inverts_on_flash() {
        let knob_style = |l: &Line| {
            l.spans
                .iter()
                .find(|s| s.content.as_ref() == "■")
                .unwrap()
                .style
        };
        // The track now carries the side via a gradient plus the knob's
        // position; the knob itself is always white and inverts on flash.
        assert_eq!(
            knob_style(&fader_line(60, FADER_LOCAL, false, 1.0)).fg,
            Some(Color::White)
        );
        assert_eq!(
            knob_style(&fader_line(60, FADER_CLOUD, false, 1.0)).fg,
            Some(Color::White)
        );
        let flashed = knob_style(&fader_line(60, 0.5, true, 1.0));
        assert_eq!(flashed.fg, Some(Color::White));
        assert!(
            flashed.add_modifier.contains(Modifier::REVERSED),
            "flash must invert the knob"
        );
    }

    #[test]
    fn gradient_lerps_between_endpoints() {
        assert_eq!(
            lerp_rgb(GRADIENT_FROM, GRADIENT_TO, 0.0),
            Color::Rgb(0, 195, 255)
        );
        assert_eq!(
            lerp_rgb(GRADIENT_FROM, GRADIENT_TO, 1.0),
            Color::Rgb(255, 60, 190)
        );
        assert_eq!(
            lerp_rgb((0, 0, 0), (200, 200, 200), 0.5),
            Color::Rgb(100, 100, 100)
        );
    }

    #[test]
    fn gradient_mode_detection_follows_env_rules() {
        // NO_COLOR wins over everything.
        assert_eq!(detect_gradient_mode(true, true), GradientMode::NoColor);
        assert_eq!(detect_gradient_mode(true, false), GradientMode::NoColor);
        // COLORTERM=truecolor/24bit enables the RGB gradient.
        assert_eq!(detect_gradient_mode(false, true), GradientMode::Truecolor);
        // Otherwise: the ANSI two-tone fallback.
        assert_eq!(detect_gradient_mode(false, false), GradientMode::Ansi);
    }

    #[test]
    fn gradient_color_degrades_gracefully() {
        // NoColor: terminal default fg — meaning carried by bold + glyphs.
        assert_eq!(
            gradient_color(GradientMode::NoColor, 0.0, 1.0),
            Color::Reset
        );
        // Ansi: two-tone crossfade between the label colors.
        assert_eq!(gradient_color(GradientMode::Ansi, 0.0, 1.0), Color::Cyan);
        assert_eq!(gradient_color(GradientMode::Ansi, 0.5, 1.0), Color::Magenta);
        // Truecolor: full gradient, faded toward black by progress.
        assert_eq!(
            gradient_color(GradientMode::Truecolor, 0.0, 1.0),
            lerp_rgb(GRADIENT_FROM, GRADIENT_TO, 0.0)
        );
        assert_eq!(
            gradient_color(GradientMode::Truecolor, 1.0, 0.0),
            Color::Rgb(0, 0, 0)
        );
    }

    #[test]
    fn fader_line_survives_degenerate_widths() {
        // Track goes to zero below the label+borders width; the knob must
        // still render and nothing may panic.
        for width in [0u16, 5, 10, 11] {
            let line = fader_line(width, 0.5, false, 1.0);
            let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
            assert!(
                text.contains('■'),
                "width {width} must keep the knob: {text}"
            );
        }
    }

    #[test]
    fn status_line_shows_spinner_while_probing_active_provider() {
        let dir = tempfile::tempdir().unwrap();
        let core = test_core(&dir);
        core.add_provider(
            Provider::new("glm", ToolKind::ClaudeCode, Some("http://gw:3000".into())),
            Some("sk-test"),
        )
        .unwrap();
        core.use_provider(ToolKind::ClaudeCode, "glm").unwrap();
        let mut app = App::new(ToolKind::ClaudeCode, &core);
        app.probing.insert("glm".to_string());

        let backend = TestBackend::new(80, 20);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|f| draw(f, &app, &[])).unwrap();
        let text = buffer_text(&terminal);
        assert!(
            SPINNER.iter().any(|g| text.contains(*g)),
            "spinner glyph must render while probing:\n{text}"
        );
        assert!(!text.contains("Latency: -"), "no dead dash:\n{text}");
    }

    #[test]
    fn tab_bar_renders_all_tools() {
        let dir = tempfile::tempdir().unwrap();
        let core = test_core(&dir);
        core.add_provider(
            Provider::new("glm", ToolKind::ClaudeCode, Some("http://gw:3000".into())),
            Some("sk-test"),
        )
        .unwrap();
        let app = App::new(ToolKind::ClaudeCode, &core);

        let backend = TestBackend::new(80, 20);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|f| draw(f, &app, &[])).unwrap();
        let text = buffer_text(&terminal);
        for t in ToolKind::ALL {
            assert!(
                text.contains(t.as_str()),
                "tab for {} missing:\n{text}",
                t.as_str()
            );
        }
    }

    #[test]
    fn latency_badge_tiers_and_timeout() {
        assert_eq!(latency_badge(&ProbeResult::Ms(12)).0, "⚡ 12ms");
        assert_eq!(latency_badge(&ProbeResult::Ms(12)).1, Color::Cyan);
        assert_eq!(latency_badge(&ProbeResult::Ms(201)).0, "● 201ms");
        assert_eq!(latency_badge(&ProbeResult::Ms(201)).1, Color::Green);
        assert_eq!(latency_badge(&ProbeResult::Ms(600)).1, Color::Yellow);
        assert_eq!(latency_badge(&ProbeResult::Ms(1500)).1, Color::Red);
        assert_eq!(latency_badge(&ProbeResult::Failed).0, "✖ timeout");
        assert_eq!(latency_badge(&ProbeResult::Failed).1, Color::Red);
    }

    #[test]
    fn list_rows_show_badge_and_success_rate() {
        let dir = tempfile::tempdir().unwrap();
        let core = test_core(&dir);
        core.add_provider(
            Provider::new("glm", ToolKind::ClaudeCode, Some("http://gw:3000".into())),
            Some("sk-test"),
        )
        .unwrap();
        core.use_provider(ToolKind::ClaudeCode, "glm").unwrap();
        let mut app = App::new(ToolKind::ClaudeCode, &core);
        app.probes.insert("glm".into(), ProbeResult::Ms(12));
        let stats = vec![StatsRow {
            group: "glm".into(),
            requests: 100,
            prompt_tokens: 0,
            completion_tokens: 0,
            errors: 2,
            avg_duration_ms: 150,
        }];

        let backend = TestBackend::new(100, 20);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|f| draw(f, &app, &stats)).unwrap();
        let text = buffer_text(&terminal);
        // ⚡ is a wide (2-column) glyph, so assert the parts separately.
        assert!(text.contains('⚡'), "badge glyph missing:\n{text}");
        assert!(text.contains("12ms"), "badge latency missing:\n{text}");
        assert!(text.contains("98% ok"), "success rate missing:\n{text}");
    }

    #[test]
    fn edit_mode_shows_fields_and_masks_api_key() {
        let dir = tempfile::tempdir().unwrap();
        let core = test_core(&dir);
        let mut p = Provider::new("glm", ToolKind::ClaudeCode, Some("http://gw:3000".into()));
        p.extra = serde_json::json!({"model": "glm-4.6"});
        core.add_provider(p, None).unwrap();
        let mut app = App::new(ToolKind::ClaudeCode, &core);
        app.start_edit();
        app.edit_next_field();
        app.edit_next_field(); // -> ApiKey
        for c in "secret".chars() {
            app.edit_char(c);
        }

        let backend = TestBackend::new(80, 20);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|f| draw(f, &app, &[])).unwrap();
        let text = buffer_text(&terminal);
        assert!(text.contains("Edit provider: glm"), "edit title:\n{text}");
        assert!(text.contains("http://gw:3000"), "base url:\n{text}");
        assert!(text.contains("glm-4.6"), "model:\n{text}");
        assert!(text.contains("******"), "masked key:\n{text}");
        assert!(!text.contains("secret"), "api key must not render in clear");
    }

    #[test]
    fn format_tokens_compacts_large_counts() {
        assert_eq!(format_tokens(999), "999");
        assert_eq!(format_tokens(1_234), "1.2k");
        assert_eq!(format_tokens(2_500_000), "2.5M");
    }

    #[test]
    fn too_small_terminal_shows_guard_message() {
        let dir = tempfile::tempdir().unwrap();
        let core = test_core(&dir);
        core.add_provider(
            Provider::new("glm", ToolKind::ClaudeCode, Some("http://gw:3000".into())),
            Some("sk-test"),
        )
        .unwrap();
        let app = App::new(ToolKind::ClaudeCode, &core);

        let backend = TestBackend::new(MIN_WIDTH - 1, MIN_HEIGHT - 1);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|f| draw(f, &app, &[])).unwrap();
        let text = buffer_text(&terminal);
        assert!(
            text.contains("terminal too small"),
            "guard message missing:\n{text}"
        );
    }

    #[test]
    fn header_title_renders_crossfade_wordmark() {
        let dir = tempfile::tempdir().unwrap();
        let core = test_core(&dir);
        core.add_provider(
            Provider::new("glm", ToolKind::ClaudeCode, Some("http://gw:3000".into())),
            Some("sk-test"),
        )
        .unwrap();
        let app = App::new(ToolKind::ClaudeCode, &core);

        let backend = TestBackend::new(80, 20);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|f| draw(f, &app, &[])).unwrap();
        let text = buffer_text(&terminal);
        assert!(
            text.contains("◀ xfade ▶"),
            "crossfade wordmark missing in title:\n{text}"
        );
    }

    #[test]
    fn selected_row_uses_reverse_video() {
        let dir = tempfile::tempdir().unwrap();
        let core = test_core(&dir);
        core.add_provider(
            Provider::new("a", ToolKind::ClaudeCode, Some("http://x".into())),
            Some("sk-test"),
        )
        .unwrap();
        core.add_provider(
            Provider::new("b", ToolKind::ClaudeCode, Some("http://y".into())),
            Some("sk-test"),
        )
        .unwrap();
        let app = App::new(ToolKind::ClaudeCode, &core);

        let backend = TestBackend::new(80, 20);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|f| draw(f, &app, &[])).unwrap();
        let buffer = terminal.backend().buffer().clone();
        let reversed = buffer
            .content()
            .iter()
            .filter(|c| c.modifier.contains(Modifier::REVERSED))
            .count();
        assert!(
            reversed > 0,
            "selected row must render reverse-video; no REVERSED cell found"
        );
    }
}
