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

use crate::tui::app::{is_local_url, App, EditField};

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
        draw_list(f, app, chunks[1]);
    }
    draw_footer(f, app, chunks[2]);
}

/// The crossfader header: local ◀───[ ■ ]───▶ cloud, plus a status line with
/// route / latency / token usage for the active provider.
fn draw_fader(f: &mut Frame, app: &App, stats: &[StatsRow], area: Rect) {
    let active = app.active_provider();
    let local = is_local_url(active.and_then(|p| p.base_url.as_deref()));
    let fader = fader_line(area.width.saturating_sub(2), local);

    let status = match active {
        Some(p) => {
            let latency = app
                .probes
                .get(&p.id)
                .map(|ms| format!("{ms}ms"))
                .or_else(|| {
                    stats
                        .iter()
                        .find(|r| r.group == p.id)
                        .map(|r| format!("avg {}ms", r.avg_duration_ms))
                })
                .unwrap_or_else(|| "-".into());
            let tokens = stats
                .iter()
                .find(|r| r.group == p.id)
                .map(|r| format_tokens((r.prompt_tokens + r.completion_tokens).max(0) as u64))
                .unwrap_or_else(|| "-".into());
            format!(
                "Route: {}   Latency: {latency}   Tokens({STATS_DAYS}): {tokens}",
                p.id
            )
        }
        None => "Route: (none)".into(),
    };

    let block = Block::default()
        .borders(Borders::ALL)
        .title(format!(" xfade · {} ", app.tool.as_str()));
    let text = Paragraph::new(vec![fader, Line::from(status)]).block(block);
    f.render_widget(text, area);
}

/// `local ◀──────[ ■ ]──────▶ cloud` with the knob on the local or cloud side.
fn fader_line(width: u16, local: bool) -> Line<'static> {
    let label_l = "local ";
    let label_r = " cloud";
    let track = (width as usize).saturating_sub(label_l.len() + label_r.len() + 2);
    let knob_pos = if local { track / 5 } else { track * 4 / 5 };
    let mut bar = String::with_capacity(track + 2);
    bar.push('◀');
    for i in 0..track {
        bar.push(if i == knob_pos { '■' } else { '─' });
    }
    bar.push('▶');
    Line::from(vec![
        Span::styled(label_l, Style::default().fg(Color::Cyan)),
        Span::raw(bar),
        Span::styled(label_r, Style::default().fg(Color::Magenta)),
    ])
}

fn draw_list(f: &mut Frame, app: &App, area: Rect) {
    let items: Vec<ListItem> = app
        .providers
        .iter()
        .enumerate()
        .map(|(i, p)| {
            let mark = if p.is_active { "*" } else { " " };
            let base = p.base_url.as_deref().unwrap_or("(official)");
            let probe = app
                .probes
                .get(&p.id)
                .map(|ms| format!("{ms}ms"))
                .unwrap_or_default();
            let line = Line::from(Span::raw(format!("{mark} {:<18} {base:<40} {probe}", p.id)));
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
    use crate::tui::app::App;
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
    fn fader_knob_moves_to_local_side_for_loopback() {
        let local = fader_line(60, true);
        let cloud = fader_line(60, false);
        let pos = |l: &Line| {
            l.spans
                .iter()
                .map(|s| s.content.as_ref())
                .collect::<String>()
                .find('■')
                .unwrap()
        };
        assert!(pos(&local) < pos(&cloud));
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
