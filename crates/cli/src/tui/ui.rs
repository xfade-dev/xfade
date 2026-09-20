//! Ratatui rendering for the TUI. All layout decisions live here; the state
//! being rendered lives in `app.rs`. Verified with `TestBackend` snapshots.

use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, Paragraph},
    Frame,
};
use xfade_core::store::db::StatsRow;
use xfade_core::ToolKind;

use crate::tui::app::{App, EditField, ProbeResult};

/// Stats window shown in the header (e.g. `7d`), parsed by `crate::parse_since`.
pub const STATS_DAYS: &str = "7d";

pub fn draw(f: &mut Frame, app: &App, stats: &[StatsRow]) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .margin(1)
        .constraints([
            Constraint::Length(5),
            Constraint::Min(3),
            Constraint::Length(3),
        ])
        .split(f.area());

    draw_fader(f, app, stats, chunks[0]);
    if app.is_editing() {
        draw_edit(f, app, chunks[1]);
    } else {
        draw_list(f, app, stats, chunks[1]);
    }
    draw_footer(f, app, chunks[2]);
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

/// The crossfader header: local ◀───[ ■ ]───▶ cloud, plus a status line with
/// route / latency / token usage for the active provider.
fn draw_fader(f: &mut Frame, app: &App, stats: &[StatsRow], area: Rect) {
    let active = app.active_provider();
    let flash = app.flash_until.is_some();
    let fader = fader_line(area.width.saturating_sub(2), app.fader_pos, flash);

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
        .title(format!(" xfade · {} ", app.tool.as_str()));
    let text = Paragraph::new(vec![tool_tabs(app), fader, status]).block(block);
    f.render_widget(text, area);
}

/// `local ◀──────[ ■ ]──────▶ cloud` with the knob at a continuous `pos`
/// (0.0 = local end, 1.0 = cloud end). Flash paints the knob white.
fn fader_line(width: u16, pos: f32, flash: bool) -> Line<'static> {
    let label_l = "local ";
    let label_r = " cloud";
    let track = (width as usize).saturating_sub(label_l.len() + label_r.len() + 2);
    let knob = ((pos.clamp(0.0, 1.0)) * track.saturating_sub(1) as f32).round() as usize;
    let left: String = "─".repeat(knob);
    let right: String = "─".repeat(track.saturating_sub(knob + 1));
    let knob_style = if flash {
        Style::default()
            .fg(Color::White)
            .add_modifier(Modifier::BOLD)
    } else if pos < 0.5 {
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default()
            .fg(Color::Magenta)
            .add_modifier(Modifier::BOLD)
    };
    Line::from(vec![
        Span::styled(label_l, Style::default().fg(Color::Cyan)),
        Span::raw("◀"),
        Span::styled(left, Style::default().fg(Color::DarkGray)),
        Span::styled("■", knob_style),
        Span::styled(right, Style::default().fg(Color::DarkGray)),
        Span::raw("▶"),
        Span::styled(label_r, Style::default().fg(Color::Magenta)),
    ])
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
            let style = if i == app.selected {
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD)
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
        let local = fader_line(60, FADER_LOCAL, false);
        let cloud = fader_line(60, FADER_CLOUD, false);
        let pos = |l: &Line| {
            l.spans
                .iter()
                .map(|s| s.content.as_ref())
                .collect::<String>()
                .find('■')
                .unwrap()
        };
        assert!(pos(&local) < pos(&cloud));
        let mid = fader_line(60, 0.5, false);
        assert!(pos(&local) < pos(&mid) && pos(&mid) < pos(&cloud));
    }

    #[test]
    fn fader_knob_color_follows_side_and_flash() {
        let knob_style = |l: &Line| {
            l.spans
                .iter()
                .find(|s| s.content.as_ref() == "■")
                .unwrap()
                .style
        };
        assert_eq!(
            knob_style(&fader_line(60, FADER_LOCAL, false)).fg,
            Some(Color::Cyan)
        );
        assert_eq!(
            knob_style(&fader_line(60, FADER_CLOUD, false)).fg,
            Some(Color::Magenta)
        );
        assert_eq!(
            knob_style(&fader_line(60, 0.5, true)).fg,
            Some(Color::White)
        );
    }

    #[test]
    fn fader_line_survives_degenerate_widths() {
        // Track goes to zero below the label+borders width; the knob must
        // still render and nothing may panic.
        for width in [0u16, 5, 10, 11] {
            let line = fader_line(width, 0.5, false);
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
}
