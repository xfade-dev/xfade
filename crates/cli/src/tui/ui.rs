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

use crate::tui::app::{AddField, App, ConfigField, EditField, ProbeResult};

/// Stats window shown in the header (e.g. `7d`), parsed by `crate::parse_since`.
pub const STATS_DAYS: &str = "7d";

/// Minimum terminal size for the full layout. Below this the header tabs and
/// provider list lose too much to stay usable, so a "too small" notice is
/// shown instead of a broken list.
const MIN_WIDTH: u16 = 30;
const MIN_HEIGHT: u16 = 13;

/// Minimum terminal width for the dual-pane layout. Below this the layout
/// degrades gracefully to a single list pane.
pub const DUAL_PANE_MIN_WIDTH: u16 = 70;
/// Minimum terminal height for the dual-pane layout. Below this the layout
/// avoids vertical sub-box collapse and renders as a single list pane.
pub const DUAL_PANE_MIN_HEIGHT: u16 = 18;

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

    if area.width >= DUAL_PANE_MIN_WIDTH && area.height >= DUAL_PANE_MIN_HEIGHT {
        let cols = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(38), Constraint::Percentage(62)])
            .split(chunks[1]);

        draw_list(f, app, stats, cols[0]);
        if app.is_input_mode() {
            draw_input(f, app, cols[1]);
        } else {
            let right_panes = Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Percentage(52), Constraint::Percentage(48)])
                .split(cols[1]);
            draw_provider_details(f, app, right_panes[0]);
            draw_gateway_metrics(f, app, stats, right_panes[1]);
        }
    } else {
        if app.is_input_mode() {
            draw_input(f, app, chunks[1]);
        } else {
            draw_list(f, app, stats, chunks[1]);
        }
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
fn title_line(
    tool: ToolKind,
    trust: xfade_core::security::TrustState,
    progress: f32,
) -> Line<'static> {
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

    let (trust_badge, trust_style) = match trust {
        xfade_core::security::TrustState::Guarded => (
            "[GUARDED]",
            Style::default()
                .fg(Color::Green)
                .add_modifier(Modifier::BOLD),
        ),
        xfade_core::security::TrustState::Direct => (
            "[DIRECT]",
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ),
        xfade_core::security::TrustState::Exposed => (
            "[EXPOSED]",
            Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
        ),
    };
    spans.push(Span::styled(format!(" {trust_badge} "), trust_style));

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
                .map(|r| {
                    format_tokens(r.prompt_tokens.saturating_add(r.completion_tokens).max(0) as u64)
                })
                .unwrap_or_else(|| "-".into());
            spans.push(Span::raw(format!("   Tokens({STATS_DAYS}): ")));
            spans.push(Span::raw(tokens));
            Line::from(spans)
        }
        None => Line::from("Route: (none)"),
    };

    let block = Block::default()
        .borders(Borders::ALL)
        .title(title_line(app.tool, app.trust, progress));
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
    if app.providers.is_empty() {
        let msg = format!(
            "No providers configured for {}.\n\nPress [a] to add a provider.",
            app.tool.as_str()
        );
        let block = Block::default()
            .borders(Borders::ALL)
            .title(format!(" Providers ({}) ", app.tool.as_str()));
        let p = Paragraph::new(msg)
            .block(block)
            .alignment(Alignment::Center)
            .wrap(Wrap { trim: true });
        f.render_widget(p, area);
        return;
    }

    let items: Vec<ListItem> = app
        .providers
        .iter()
        .enumerate()
        .map(|(i, p)| {
            let mark = if p.is_active { "*" } else { " " };
            let base = p.base_url.as_deref().unwrap_or("(official)");
            let mut spans = if area.width >= 55 {
                vec![Span::raw(format!("{mark} {:<14} {:<20} ", p.id, base))]
            } else {
                vec![Span::raw(format!("{mark} {:<13} ", p.id))]
            };
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

fn draw_provider_details(f: &mut Frame, app: &App, area: Rect) {
    let block = Block::default()
        .borders(Borders::ALL)
        .title(match app.selected_provider() {
            Some(p) => format!(" Provider Details: {} ", p.id),
            None => " Provider Details ".to_string(),
        });

    let Some(p) = app.selected_provider() else {
        let p = Paragraph::new("No provider selected.").block(block);
        f.render_widget(p, area);
        return;
    };

    let base_url = p.base_url.as_deref().unwrap_or("(official vendor login)");
    let protocol = match p.protocol {
        xfade_core::ProviderProtocol::Chat => "Chat (OpenAI-compatible)",
        xfade_core::ProviderProtocol::Messages => "Messages (Anthropic)",
        xfade_core::ProviderProtocol::Responses => "Responses (OpenAI v1/responses)",
    };
    let model = p
        .extra
        .get("model")
        .and_then(|v| v.as_str())
        .unwrap_or("(default model)");

    let mut lines = vec![
        Line::from(vec![
            Span::styled("  • Endpoint:   ", Style::default().fg(Color::DarkGray)),
            Span::styled(base_url, Style::default().add_modifier(Modifier::BOLD)),
        ]),
        Line::from(vec![
            Span::styled("  • Protocol:   ", Style::default().fg(Color::DarkGray)),
            Span::styled(protocol, Style::default().fg(Color::Cyan)),
        ]),
        Line::from(vec![
            Span::styled("  • Model:      ", Style::default().fg(Color::DarkGray)),
            Span::styled(model, Style::default().fg(Color::White)),
        ]),
    ];

    // Optional slot overrides (e.g. for Claude Code)
    let opus = p.extra.get("opus_model").and_then(|v| v.as_str());
    let sonnet = p.extra.get("sonnet_model").and_then(|v| v.as_str());
    let haiku = p.extra.get("haiku_model").and_then(|v| v.as_str());
    if opus.is_some() || sonnet.is_some() || haiku.is_some() {
        let mut slot_parts = Vec::new();
        if let Some(s) = haiku {
            slot_parts.push(format!("haiku:{s}"));
        }
        if let Some(s) = sonnet {
            slot_parts.push(format!("sonnet:{s}"));
        }
        if let Some(s) = opus {
            slot_parts.push(format!("opus:{s}"));
        }
        lines.push(Line::from(vec![
            Span::styled("  • Slots:      ", Style::default().fg(Color::DarkGray)),
            Span::styled(slot_parts.join(" · "), Style::default().fg(Color::Yellow)),
        ]));
    }

    let status_str = if p.is_active {
        Span::styled(
            "ACTIVE (Serving tool)",
            Style::default()
                .fg(Color::Green)
                .add_modifier(Modifier::BOLD),
        )
    } else {
        Span::styled("STANDBY", Style::default().fg(Color::DarkGray))
    };
    lines.push(Line::from(vec![
        Span::styled("  • Status:     ", Style::default().fg(Color::DarkGray)),
        status_str,
    ]));

    let key_ref = format!("OS Keychain [ref:{}]", p.key_ref);
    lines.push(Line::from(vec![
        Span::styled("  • Credential: ", Style::default().fg(Color::DarkGray)),
        Span::styled(key_ref, Style::default().fg(Color::DarkGray)),
    ]));

    let paragraph = Paragraph::new(lines).block(block);
    f.render_widget(paragraph, area);
}

fn draw_gateway_metrics(f: &mut Frame, app: &App, stats: &[StatsRow], area: Rect) {
    let block = Block::default()
        .borders(Borders::ALL)
        .title(" Gateway & Traffic Health ");

    let Some(p) = app.selected_provider() else {
        let p = Paragraph::new("No provider selected.").block(block);
        f.render_widget(p, area);
        return;
    };

    let maybe_stat = stats.iter().find(|r| r.group == p.id);
    let mut lines = Vec::new();

    // Trust badge line
    let (trust_badge, trust_color) = match app.trust {
        xfade_core::security::TrustState::Guarded => {
            ("GUARDED (Zero raw keys on disk)", Color::Green)
        }
        xfade_core::security::TrustState::Direct => {
            ("DIRECT (Escape hatch enabled)", Color::Yellow)
        }
        xfade_core::security::TrustState::Exposed => {
            ("EXPOSED (Security review needed)", Color::Red)
        }
    };
    lines.push(Line::from(vec![
        Span::styled("  • Security:   ", Style::default().fg(Color::DarkGray)),
        Span::styled(
            trust_badge,
            Style::default()
                .fg(trust_color)
                .add_modifier(Modifier::BOLD),
        ),
    ]));

    if let Some(row) = maybe_stat {
        let ok = if row.requests > 0 {
            100.0 * (1.0 - row.errors as f64 / row.requests as f64)
        } else {
            100.0
        };
        let ok_color = if ok >= 95.0 { Color::Green } else { Color::Red };

        lines.push(Line::from(vec![
            Span::styled("  • Requests:   ", Style::default().fg(Color::DarkGray)),
            Span::styled(
                format!("{} reqs", row.requests),
                Style::default().add_modifier(Modifier::BOLD),
            ),
            Span::styled(format!(" ({ok:.1}% ok)"), Style::default().fg(ok_color)),
        ]));

        lines.push(Line::from(vec![
            Span::styled("  • Latency:    ", Style::default().fg(Color::DarkGray)),
            Span::styled(format!("avg {}ms", row.avg_duration_ms), Style::default()),
            Span::styled(
                format!(
                    " · {} tokens",
                    format_tokens(
                        row.prompt_tokens
                            .saturating_add(row.completion_tokens)
                            .max(0) as u64
                    )
                ),
                Style::default().fg(Color::DarkGray),
            ),
        ]));

        let cost = row.cost_usd_micros as f64 / 1_000_000.0;
        lines.push(Line::from(vec![
            Span::styled("  • Est. Cost:  ", Style::default().fg(Color::DarkGray)),
            Span::styled(
                format!("${cost:.4} (est)"),
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            ),
        ]));
    } else {
        lines.push(Line::from(vec![
            Span::styled("  • Traffic:    ", Style::default().fg(Color::DarkGray)),
            Span::styled(
                "No proxy calls recorded in last 7d",
                Style::default().fg(Color::DarkGray),
            ),
        ]));
        lines.push(Line::from(vec![
            Span::styled("  • Circuit:    ", Style::default().fg(Color::DarkGray)),
            Span::styled(
                "CLOSED (Healthy / Standby)",
                Style::default().fg(Color::Green),
            ),
        ]));
    }

    let paragraph = Paragraph::new(lines).block(block);
    f.render_widget(paragraph, area);
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
                "*".repeat(value.chars().count())
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

fn draw_input(f: &mut Frame, app: &App, area: Rect) {
    if app.is_adding() {
        draw_add(f, app, area);
    } else if app.is_configuring() {
        draw_config(f, app, area);
    } else {
        draw_edit(f, app, area);
    }
}

/// One form row. `masked` hides a typed secret; `placeholder` marks `value`
/// as hint text (dimmed) rather than something the user typed.
fn input_line(
    label: &str,
    value: String,
    focused: bool,
    masked: bool,
    placeholder: bool,
) -> Line<'static> {
    let shown = if masked && !placeholder && !value.is_empty() {
        "*".repeat(value.chars().count())
    } else {
        value
    };
    let style = if placeholder {
        Style::default().fg(Color::DarkGray)
    } else if focused {
        Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default()
    };
    Line::from(vec![
        Span::raw(format!("{label:<12}")),
        Span::styled(shown, style),
        if focused {
            Span::styled("_", Style::default().fg(Color::Yellow))
        } else {
            Span::raw("")
        },
    ])
}

/// Render a form, scrolling so the focused row stays visible. Short terminals
/// (the single-pane layout can leave only a few rows) would otherwise clip the
/// field being edited.
fn render_form(f: &mut Frame, area: Rect, title: String, lines: Vec<Line<'static>>, focus: usize) {
    let inner_height = area.height.saturating_sub(2) as usize;
    let scroll = (focus + 1).saturating_sub(inner_height) as u16;
    let block = Block::default().borders(Borders::ALL).title(title);
    f.render_widget(Paragraph::new(lines).block(block).scroll((scroll, 0)), area);
}

fn draw_add(f: &mut Frame, app: &App, area: Rect) {
    let Some(add) = app.add.as_ref() else {
        return;
    };
    // Blank fields show the default Core will apply, dimmed.
    let (url, url_hint) = match (add.base_url.is_empty(), add.global_base_url.as_deref()) {
        (true, Some(g)) => (format!("(blank = global: {g})"), true),
        (true, None) => ("(required unless Official)".to_string(), true),
        (false, _) => (add.base_url.clone(), false),
    };
    let (model, model_hint) = match (add.model.is_empty(), add.global_model.as_deref()) {
        (true, Some(g)) => (format!("(blank = global: {g})"), true),
        (true, None) => ("(provider default)".to_string(), true),
        (false, _) => (add.model.clone(), false),
    };
    let key_hint = add.api_key.is_empty();
    let proto_hint = add.protocol.is_empty();
    let lines = vec![
        input_line(
            "ID:",
            add.id.clone(),
            add.field == AddField::Id,
            false,
            false,
        ),
        input_line(
            "Base URL:",
            url,
            add.field == AddField::BaseUrl,
            false,
            url_hint,
        ),
        input_line(
            "Model:",
            model,
            add.field == AddField::Model,
            false,
            model_hint,
        ),
        input_line(
            "API Key:",
            if key_hint {
                "(blank = global key)".into()
            } else {
                add.api_key.clone()
            },
            add.field == AddField::ApiKey,
            true,
            key_hint,
        ),
        input_line(
            "Protocol:",
            if proto_hint {
                "(auto)".into()
            } else {
                add.protocol.clone()
            },
            add.field == AddField::Protocol,
            false,
            proto_hint,
        ),
        input_line(
            "Official:",
            if add.official { "[x]" } else { "[ ]" }.into(),
            add.field == AddField::Official,
            false,
            false,
        ),
        Line::from(Span::styled(
            "Protocol: chat/messages/responses · Space toggles Official",
            Style::default().fg(Color::DarkGray),
        )),
    ];
    let focus = match add.field {
        AddField::Id => 0,
        AddField::BaseUrl => 1,
        AddField::Model => 2,
        AddField::ApiKey => 3,
        AddField::Protocol => 4,
        AddField::Official => 5,
    };
    render_form(
        f,
        area,
        format!(" Add provider for {} ", app.tool.as_str()),
        lines,
        focus,
    );
}

fn draw_config(f: &mut Frame, app: &App, area: Rect) {
    let Some(config) = app.config_edit.as_ref() else {
        return;
    };
    let key_empty = config.api_key.is_empty();
    let key_text = if !key_empty {
        config.api_key.clone()
    } else if config.api_key_is_set {
        "(blank = keep current key)".to_string()
    } else {
        "(blank = unset)".to_string()
    };
    let lines = vec![
        input_line(
            "Secrets:",
            config.secrets.clone(),
            config.field == ConfigField::Secrets,
            false,
            false,
        ),
        input_line(
            "Base URL:",
            config.base_url.clone(),
            config.field == ConfigField::BaseUrl,
            false,
            false,
        ),
        input_line(
            "Model:",
            config.model.clone(),
            config.field == ConfigField::Model,
            false,
            false,
        ),
        input_line(
            "API:",
            config.api.clone(),
            config.field == ConfigField::Api,
            false,
            false,
        ),
        input_line(
            "API Key:",
            key_text,
            config.field == ConfigField::ApiKey,
            true,
            key_empty,
        ),
        Line::from(""),
        Line::from(Span::styled(
            "Creation-time defaults only · backend change requires restart",
            Style::default().fg(Color::DarkGray),
        )),
    ];
    let focus = match config.field {
        ConfigField::Secrets => 0,
        ConfigField::BaseUrl => 1,
        ConfigField::Model => 2,
        ConfigField::Api => 3,
        ConfigField::ApiKey => 4,
    };
    render_form(f, area, " Global defaults ".to_string(), lines, focus);
}

fn draw_footer(f: &mut Frame, app: &App, area: Rect) {
    let hints = if app.delete_confirm.is_some() {
        "confirm removal: [y] yes   [n/Esc] cancel"
    } else if app.is_input_mode() {
        "editing: [Tab/BackTab] field   [Enter] save   [Esc] cancel"
    } else {
        "j/k move  Enter use  a add  e edit  d del  g global  t test  Tab tool  q quit"
    };
    let mut lines = vec![Line::from(Span::styled(
        hints,
        Style::default().fg(Color::DarkGray),
    ))];
    if let Some(m) = &app.message {
        lines.push(Line::from(Span::styled(
            m.clone(),
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        )));
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
        std::env::set_var("XFADE_TEST_SKIP_SPAWN", "1");
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
    fn test_compact_terminal_degrades_to_single_pane() {
        let dir = tempfile::tempdir().unwrap();
        let core = test_core(&dir);
        core.add_provider(
            Provider::new("glm", ToolKind::ClaudeCode, Some("http://gw:3000".into())),
            Some("sk-test"),
        )
        .unwrap();
        let app = App::new(ToolKind::ClaudeCode, &core);

        let backend = TestBackend::new(60, 20); // Width 60 < 70
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|f| draw(f, &app, &[])).unwrap();
        let text = buffer_text(&terminal);
        assert!(
            !text.contains("Provider Details:"),
            "should not render right pane in narrow mode:\n{text}"
        );
        assert!(
            text.contains("Providers (claude)"),
            "should render list:\n{text}"
        );
    }

    #[test]
    fn test_dual_pane_degrades_when_short_height() {
        let dir = tempfile::tempdir().unwrap();
        let core = test_core(&dir);
        core.add_provider(
            Provider::new("glm", ToolKind::ClaudeCode, Some("http://gw:3000".into())),
            Some("sk-test"),
        )
        .unwrap();
        let app = App::new(ToolKind::ClaudeCode, &core);

        let backend = TestBackend::new(80, 15); // Width 80 >= 70, but Height 15 < 18
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|f| draw(f, &app, &[])).unwrap();
        let text = buffer_text(&terminal);
        assert!(
            !text.contains("Provider Details:"),
            "should degrade vertically to single pane:\n{text}"
        );
        assert!(
            text.contains("Providers (claude)"),
            "should render list:\n{text}"
        );
    }

    #[test]
    fn test_empty_provider_list_renders_clear_message() {
        let dir = tempfile::tempdir().unwrap();
        let core = test_core(&dir);
        let app = App::new(ToolKind::ClaudeCode, &core); // No providers added

        let backend = TestBackend::new(80, 20);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|f| draw(f, &app, &[])).unwrap();
        let text = buffer_text(&terminal);
        assert!(
            text.contains("No providers configured for"),
            "empty msg:\n{text}"
        );
        assert!(
            text.contains("Press [a] to add a provider"),
            "actionable instructions:\n{text}"
        );
    }

    #[test]
    fn test_token_saturation_large_numbers() {
        let dir = tempfile::tempdir().unwrap();
        let core = test_core(&dir);
        let p = Provider::new("sat", ToolKind::ClaudeCode, Some("http://sat:3000".into()));
        core.add_provider(p, Some("sk-sat")).unwrap();
        core.use_provider(ToolKind::ClaudeCode, "sat", true)
            .unwrap();
        let app = App::new(ToolKind::ClaudeCode, &core);

        let stats = vec![StatsRow {
            group: "sat".into(),
            requests: 1,
            prompt_tokens: i64::MAX - 10,
            completion_tokens: 100, // Would overflow i64 without saturating_add
            cost_usd_micros: 1000,
            errors: 0,
            avg_duration_ms: 10,
        }];

        let backend = TestBackend::new(100, 25);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|f| draw(f, &app, &stats)).unwrap();
        let text = buffer_text(&terminal);
        assert!(
            text.contains("Tokens(7d):"),
            "header tokens present:\n{text}"
        );
    }

    #[test]
    fn test_dual_pane_renders_details_and_metrics_when_wide() {
        let dir = tempfile::tempdir().unwrap();
        let core = test_core(&dir);
        let mut p = Provider::new(
            "deepseek",
            ToolKind::ClaudeCode,
            Some("https://api.deepseek.com/v1".into()),
        );
        p.protocol = xfade_core::ProviderProtocol::Chat;
        p.extra = serde_json::json!({"model": "deepseek-chat"});
        core.add_provider(p, Some("sk-secret")).unwrap();
        core.use_provider(ToolKind::ClaudeCode, "deepseek", true)
            .unwrap();

        let app = App::new(ToolKind::ClaudeCode, &core);
        let stats = vec![StatsRow {
            group: "deepseek".into(),
            requests: 50,
            prompt_tokens: 10_000,
            completion_tokens: 2_000,
            cost_usd_micros: 35_000, // $0.035
            errors: 0,
            avg_duration_ms: 220,
        }];

        let backend = TestBackend::new(100, 25);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|f| draw(f, &app, &stats)).unwrap();
        let text = buffer_text(&terminal);
        assert!(
            text.contains("Provider Details: deepseek"),
            "details title:\n{text}"
        );
        assert!(
            text.contains("https://api.deepseek.com/v1"),
            "endpoint:\n{text}"
        );
        assert!(text.contains("Chat"), "protocol:\n{text}");
        assert!(
            text.contains("Gateway & Traffic Health"),
            "metrics title:\n{text}"
        );
        assert!(text.contains("$0.0350"), "estimated cost:\n{text}");
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
        core.use_provider(ToolKind::ClaudeCode, "glmcc", true)
            .unwrap();
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
        core.use_provider(ToolKind::ClaudeCode, "glm", true)
            .unwrap();
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
        core.use_provider(ToolKind::ClaudeCode, "glm", true)
            .unwrap();
        let mut app = App::new(ToolKind::ClaudeCode, &core);
        app.probes.insert("glm".into(), ProbeResult::Ms(12));
        let stats = vec![StatsRow {
            group: "glm".into(),
            requests: 100,
            prompt_tokens: 0,
            completion_tokens: 0,
            cost_usd_micros: 0,
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
    fn add_mode_renders_cli_synced_provider_fields() {
        let dir = tempfile::tempdir().unwrap();
        let core = test_core(&dir);
        let mut app = App::new(ToolKind::ClaudeCode, &core);
        app.start_add(&core);

        let backend = TestBackend::new(100, 25);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|f| draw(f, &app, &[])).unwrap();
        let text = buffer_text(&terminal);
        assert!(text.contains("Add provider for claude"), "{text}");
        assert!(text.contains("Protocol:"), "{text}");
        assert!(text.contains("Official:"), "{text}");
        assert!(text.contains("blank = global key"), "{text}");
    }

    #[test]
    fn add_mode_shows_global_defaults_as_placeholders() {
        let dir = tempfile::tempdir().unwrap();
        let core = test_core(&dir);
        core.update_config(None, Some("https://g.example"), Some("g-model"), None)
            .unwrap();
        let mut app = App::new(ToolKind::ClaudeCode, &core);
        app.start_add(&core);

        let backend = TestBackend::new(100, 25);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|f| draw(f, &app, &[])).unwrap();
        let text = buffer_text(&terminal);
        assert!(text.contains("blank = global: https://g.example"), "{text}");
        assert!(text.contains("blank = global: g-model"), "{text}");
    }

    #[test]
    fn form_scrolls_to_keep_focused_field_visible_in_short_terminal() {
        let dir = tempfile::tempdir().unwrap();
        let core = test_core(&dir);
        let mut app = App::new(ToolKind::ClaudeCode, &core);
        app.start_add(&core);
        for _ in 0..5 {
            app.edit_next_field(); // -> Official, the 6th row
        }
        // Minimum height leaves only a few rows for the form.
        let backend = TestBackend::new(60, MIN_HEIGHT);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|f| draw(f, &app, &[])).unwrap();
        let text = buffer_text(&terminal);
        assert!(text.contains("Official:"), "focused row clipped:\n{text}");
    }

    #[test]
    fn footer_hints_fit_in_80_columns() {
        let dir = tempfile::tempdir().unwrap();
        let core = test_core(&dir);
        let app = App::new(ToolKind::ClaudeCode, &core);
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|f| draw(f, &app, &[])).unwrap();
        let text = buffer_text(&terminal);
        assert!(text.contains("q quit"), "footer truncated:\n{text}");
    }

    #[test]
    fn global_config_mode_labels_values_as_creation_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let core = test_core(&dir);
        let mut app = App::new(ToolKind::ClaudeCode, &core);
        app.start_config(&core);

        let backend = TestBackend::new(100, 25);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|f| draw(f, &app, &[])).unwrap();
        let text = buffer_text(&terminal);
        assert!(text.contains("Global defaults"), "{text}");
        assert!(text.contains("Creation-time defaults only"), "{text}");
        assert!(text.contains("Secrets:"), "{text}");
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
