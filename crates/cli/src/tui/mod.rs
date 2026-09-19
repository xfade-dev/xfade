pub mod app;
pub mod ui;

use crossterm::{
    event::{self, Event, KeyCode, KeyEventKind},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{backend::CrosstermBackend, Terminal};
use std::io::{self, IsTerminal};
use std::sync::mpsc;
use std::time::Duration;
use xfade_core::store::db::{StatsGroupBy, StatsRow};
use xfade_core::{Core, CoreError, Result, ToolKind};

use app::{probe_latency, probe_target, App};

/// Run the interactive TUI for a given tool.
///
/// Keys (list mode): j/k or arrows to move, Enter to switch, `t` to probe the
/// selected provider's latency, `e` to edit it, Tab to cycle tools, q to quit.
///
/// Non-TTY degradation: when stdin/stdout is not a terminal (pipes, CI,
/// redirect), print the plain provider list instead of failing inside
/// crossterm's input reader.
///
/// Terminal safety: raw mode + alternate screen are restored via a panic hook,
/// so a panic never leaves the user with a hidden cursor / garbled terminal.
pub fn run_tui(core: &Core, tool: ToolKind) -> Result<()> {
    if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
        return print_plain_list(core, tool);
    }

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

    let result = event_loop(core, tool, &mut terminal);

    // ── Terminal cleanup ──────────────────────────────────────────────
    disable_raw_mode().map_err(|e| CoreError::Proxy(format!("disable raw mode: {e}")))?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)
        .map_err(|e| CoreError::Proxy(format!("leave alternate screen: {e}")))?;
    terminal
        .show_cursor()
        .map_err(|e| CoreError::Proxy(format!("show cursor: {e}")))?;

    // Drop the TUI panic hook so later panics don't write terminal escapes.
    let _ = std::panic::take_hook();

    result
}

/// Non-interactive fallback mirroring `xfade ls` output.
fn print_plain_list(core: &Core, tool: ToolKind) -> Result<()> {
    let providers = core.list(Some(tool))?;
    if providers.is_empty() {
        println!("no providers for {}; run `xfade add`", tool.as_str());
        return Ok(());
    }
    for p in providers {
        let mark = if p.is_active { "*" } else { " " };
        let base = p.base_url.as_deref().unwrap_or("(official)");
        println!("{mark} {:<10} {:<18} {base}", tool.as_str(), p.id);
    }
    Ok(())
}

fn event_loop(
    core: &Core,
    tool: ToolKind,
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
) -> Result<()> {
    let mut app = App::new(tool, core);
    let mut stats = load_stats(core);

    // Latency probes run in worker threads; results arrive over this channel.
    let (probe_tx, probe_rx) = mpsc::channel::<(String, Option<u128>)>();
    let mut probing = false;

    loop {
        while let Ok((id, ms)) = probe_rx.try_recv() {
            probing = false;
            match ms {
                Some(ms) => {
                    app.probes.insert(id.clone(), ms);
                    app.message = Some(format!("{id}: {ms}ms"));
                }
                None => app.message = Some(format!("{id}: unreachable")),
            }
        }

        terminal
            .draw(|f| ui::draw(f, &app, &stats))
            .map_err(|e| CoreError::Proxy(format!("draw: {e}")))?;

        // Poll with a timeout so probe results repaint without a keypress.
        if !event::poll(Duration::from_millis(100))
            .map_err(|e| CoreError::Proxy(format!("poll event: {e}")))?
        {
            continue;
        }
        let Event::Key(key) =
            event::read().map_err(|e| CoreError::Proxy(format!("read event: {e}")))?
        else {
            continue;
        };
        if key.kind != KeyEventKind::Press {
            continue;
        }

        if app.is_editing() {
            match key.code {
                KeyCode::Esc => app.cancel_edit(),
                KeyCode::Enter => {
                    app.submit_edit(core);
                    stats = load_stats(core);
                }
                KeyCode::Tab | KeyCode::Down => app.edit_next_field(),
                KeyCode::Up => app.edit_prev_field(),
                KeyCode::Backspace => app.edit_backspace(),
                KeyCode::Char(c) => app.edit_char(c),
                _ => {}
            }
            continue;
        }

        match key.code {
            KeyCode::Char('q') | KeyCode::Esc => break,
            KeyCode::Char('j') | KeyCode::Down => app.next(),
            KeyCode::Char('k') | KeyCode::Up => app.prev(),
            KeyCode::Tab => app.next_tool(core),
            KeyCode::Enter => {
                app.switch_selected(core);
                stats = load_stats(core);
            }
            KeyCode::Char('e') => app.start_edit(),
            KeyCode::Char('t') => {
                if probing {
                    app.message = Some("probe already running…".into());
                } else if let Some(p) = app.selected_provider() {
                    match p.base_url.as_deref().and_then(probe_target) {
                        Some((host, port)) => {
                            let id = p.id.clone();
                            let tx = probe_tx.clone();
                            probing = true;
                            app.message = Some(format!("probing {id}…"));
                            std::thread::spawn(move || {
                                let ms = probe_latency(&host, port, Duration::from_secs(3));
                                let _ = tx.send((id, ms));
                            });
                        }
                        None => app.message = Some(format!("{} has no probeable base_url", p.id)),
                    }
                }
            }
            _ => {}
        }
    }
    Ok(())
}

/// Token/latency aggregates for the status line, over the last `STATS_DAYS`.
fn load_stats(core: &Core) -> Vec<StatsRow> {
    let Ok(since) = crate::parse_since(ui::STATS_DAYS) else {
        return Vec::new();
    };
    core.db()
        .stats_since(&since, StatsGroupBy::Provider)
        .unwrap_or_default()
}
