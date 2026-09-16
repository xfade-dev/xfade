use crossterm::{
    event::{self, Event, KeyCode, KeyEventKind},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, Paragraph},
    Terminal,
};
use std::io;
use xfade_core::{Core, CoreError, Result, ToolKind};

/// Run the interactive TUI for a given tool: navigate providers with j/k or arrows,
/// Enter to switch, q to quit.
///
/// Terminal safety: raw mode + alternate screen are restored via a panic hook, so
/// a panic (or a Ctrl+C that a future version intercepts) never leaves the user
/// with a hidden cursor / garbled terminal.
pub fn run_tui(core: &Core, tool: ToolKind) -> Result<()> {
    // ── Terminal setup ────────────────────────────────────────────────
    enable_raw_mode().map_err(|e| CoreError::Proxy(format!("enable raw mode: {e}")))?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)
        .map_err(|e| CoreError::Proxy(format!("enter alternate screen: {e}")))?;

    // Restore the terminal on panic so it never stays garbled / cursor-hidden.
    let original_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = disable_raw_mode();
        let _ = execute!(io::stdout(), LeaveAlternateScreen);
        original_hook(info);
    }));

    let backend = CrosstermBackend::new(stdout);
    let mut terminal =
        Terminal::new(backend).map_err(|e| CoreError::Proxy(format!("init terminal: {e}")))?;

    let mut providers = core.list(Some(tool)).unwrap_or_default();
    let mut selected = providers.iter().position(|p| p.is_active).unwrap_or(0);
    let mut message: Option<String> = None;

    loop {
        // Refresh list each frame (a switch updates the active flags).
        providers = core.list(Some(tool)).unwrap_or_default();
        if selected >= providers.len() {
            selected = providers.len().saturating_sub(1);
        }
        let active = providers.iter().find(|p| p.is_active).map(|p| p.id.clone());

        terminal.draw(|f| {
            let chunks = Layout::default()
                .direction(Direction::Vertical)
                .margin(1)
                .constraints([
                    Constraint::Length(3),
                    Constraint::Min(1),
                    Constraint::Length(3),
                ])
                .split(f.area());

            // Header: title + active provider / last message.
            let title = format!("xfade · {tool}");
            let active_line = match (&active, &message) {
                (Some(a), _) => format!("Active: {a}"),
                (None, Some(m)) => m.clone(),
                (None, None) => "Active: (none)".to_string(),
            };
            let header = Paragraph::new(vec![
                Line::from(Span::styled(
                    title,
                    Style::default().add_modifier(Modifier::BOLD),
                )),
                Line::from(active_line),
            ])
            .block(Block::default().borders(Borders::ALL));
            f.render_widget(header, chunks[0]);

            // Provider list.
            let items: Vec<ListItem> = providers
                .iter()
                .enumerate()
                .map(|(i, p)| {
                    let mark = if p.is_active { "*" } else { " " };
                    let base = p.base_url.as_deref().unwrap_or("(official)");
                    let line = Line::from(Span::raw(format!("{mark} {:<18} {base}", p.id)));
                    let style = if i == selected {
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
                    .title(format!(" Providers ({tool}) ")),
            );
            f.render_widget(list, chunks[1]);

            // Footer: key hints.
            let footer = Paragraph::new("[↑/↓ or j/k] move   [Enter] switch   [q] quit")
                .style(Style::default().fg(Color::DarkGray));
            f.render_widget(footer, chunks[2]);
        })?;

        // Input.
        if let Event::Key(key) =
            event::read().map_err(|e| CoreError::Proxy(format!("read event: {e}")))?
        {
            if key.kind != KeyEventKind::Press {
                continue;
            }
            match key.code {
                KeyCode::Char('q') | KeyCode::Esc => break,
                KeyCode::Char('j') | KeyCode::Down => {
                    if !providers.is_empty() {
                        selected = (selected + 1) % providers.len();
                    }
                }
                KeyCode::Char('k') | KeyCode::Up => {
                    if !providers.is_empty() {
                        selected = (selected + providers.len() - 1) % providers.len();
                    }
                }
                KeyCode::Enter => {
                    if let Some(p) = providers.get(selected) {
                        let id = p.id.clone();
                        match core.use_provider(tool, &id) {
                            Ok(()) => message = Some(format!("Switched to {id}")),
                            Err(e) => message = Some(format!("Error: {e}")),
                        }
                    }
                }
                _ => {}
            }
        }
    }

    // ── Terminal cleanup ──────────────────────────────────────────────
    disable_raw_mode().map_err(|e| CoreError::Proxy(format!("disable raw mode: {e}")))?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)
        .map_err(|e| CoreError::Proxy(format!("leave alternate screen: {e}")))?;
    terminal
        .show_cursor()
        .map_err(|e| CoreError::Proxy(format!("show cursor: {e}")))?;

    Ok(())
}
