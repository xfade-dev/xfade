pub mod app;
pub mod ui;

use crossterm::{
    event::{self, Event, KeyCode, KeyEventKind, KeyModifiers},
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
/// selected provider's latency, `e` to edit it, Tab to cycle tools, q or
/// Ctrl+C to quit.
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
    let mut stats = load_stats(core, &app.tool);

    // Latency probes run in worker threads; results arrive over this channel.
    let (probe_tx, probe_rx) = mpsc::channel::<(String, Option<u128>)>();
    // Accumulators for the end-of-round summary message.
    let (mut probe_ok, mut probe_fail, mut probe_worst) = (0u32, 0u32, 0u128);

    // Redraw only when state changed or an animation is in flight, so an idle
    // terminal doesn't repaint at the poll cadence.
    let mut dirty = true;
    loop {
        let mut drained = false;
        while let Ok((id, ms)) = probe_rx.try_recv() {
            app.probing.remove(&id);
            match ms {
                Some(ms) => {
                    app.probes.insert(id.clone(), app::ProbeResult::Ms(ms));
                    probe_ok += 1;
                    probe_worst = probe_worst.max(ms);
                }
                None => {
                    app.probes.insert(id.clone(), app::ProbeResult::Failed);
                    probe_fail += 1;
                }
            }
            drained = true;
        }
        // Per-provider badges carry the detail; the message line only needs a
        // summary once the whole round finishes (results arrive out of order,
        // so per-result messages would just overwrite each other).
        if drained {
            dirty = true;
            if app.probing.is_empty() {
                let n = probe_ok + probe_fail;
                app.message = Some(if probe_fail == 0 {
                    format!("{probe_ok}/{n} ok · worst {probe_worst}ms")
                } else if probe_ok == 0 {
                    format!("all {n} unreachable")
                } else {
                    format!("{probe_ok}/{n} ok · {probe_fail} down · worst {probe_worst}ms")
                });
                probe_ok = 0;
                probe_fail = 0;
                probe_worst = 0;
            }
        }

        // Repaint while the fader/spinner is animating, and on the frame where
        // it settles so the knob lands exactly on target.
        let was_animating = app.is_animating();
        app.tick();
        if was_animating || app.is_animating() {
            dirty = true;
        }

        if dirty {
            terminal
                .draw(|f| ui::draw(f, &app, &stats))
                .map_err(|e| CoreError::Proxy(format!("draw: {e}")))?;
            dirty = false;
        }

        // Poll with a timeout so probe results repaint without a keypress.
        // While the fader is sliding or a probe spinner is up, poll fast for
        // smooth frames; otherwise idle at a slower cadence.
        let poll_ms = if app.is_animating() { 16 } else { 100 };
        if !event::poll(Duration::from_millis(poll_ms))
            .map_err(|e| CoreError::Proxy(format!("poll event: {e}")))?
        {
            continue;
        }
        let Event::Key(key) =
            event::read().map_err(|e| CoreError::Proxy(format!("read event: {e}")))?
        else {
            // Non-key events (notably Resize) must trigger a repaint at the
            // new terminal size; otherwise the dirty flag never gets set and
            // the UI keeps showing the stale layout.
            dirty = true;
            continue;
        };
        if key.kind != KeyEventKind::Press {
            continue;
        }

        // Any key press counts as interaction: repaint next frame, and drop
        // the slow opening glide so mid-intro switches stay snappy.
        dirty = true;
        app.user_interacted = true;

        // Ctrl+C must always quit cleanly: raw mode turns SIGINT into a key
        // event (Char('c') + CONTROL), so it never reaches the default signal
        // handler on its own.
        if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
            break;
        }

        if app.is_editing() {
            match key.code {
                KeyCode::Esc => app.cancel_edit(),
                KeyCode::Enter => {
                    app.submit_edit(core);
                    stats = load_stats(core, &app.tool);
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
            KeyCode::Tab => {
                app.next_tool(core);
                stats = load_stats(core, &app.tool);
            }
            KeyCode::Enter => {
                app.switch_selected(core);
                stats = load_stats(core, &app.tool);
            }
            KeyCode::Char('e') => app.start_edit(),
            KeyCode::Char('t') => {
                if !app.probing.is_empty() {
                    app.message = Some("probe already running…".into());
                } else {
                    // Probe every provider of the current tool in parallel.
                    // Official-login providers (no base_url) are skipped, as
                    // are base_urls that fail to parse (counted separately).
                    let mut targets: Vec<(String, String, u16)> = Vec::new();
                    let mut unparseable = 0usize;
                    for p in &app.providers {
                        match p.base_url.as_deref().and_then(probe_target) {
                            Some((host, port)) => targets.push((p.id.clone(), host, port)),
                            None if p.base_url.is_some() => unparseable += 1,
                            None => {}
                        }
                    }
                    if targets.is_empty() {
                        app.message = Some(if unparseable > 0 {
                            format!("no probeable base_url ({unparseable} unparseable)")
                        } else {
                            "no probeable provider (all official)".into()
                        });
                    } else {
                        let n = targets.len();
                        for (id, host, port) in targets {
                            let tx = probe_tx.clone();
                            app.probing.insert(id.clone());
                            std::thread::spawn(move || {
                                let ms = probe_latency(&host, port, Duration::from_secs(3));
                                let _ = tx.send((id, ms));
                            });
                        }
                        app.message = Some(format!("probing {n} providers…"));
                    }
                }
            }
            _ => {}
        }
    }
    Ok(())
}

/// Token/latency aggregates for the status line, over the last `STATS_DAYS`.
/// Scoped to `tool`: provider ids can collide across tools, so an unfiltered
/// query would bleed another tool's numbers into this view.
fn load_stats(core: &Core, tool: &ToolKind) -> Vec<StatsRow> {
    let Ok(since) = crate::parse_since(ui::STATS_DAYS) else {
        return Vec::new();
    };
    core.db()
        .stats_since(&since, StatsGroupBy::Provider, Some(tool))
        .unwrap_or_default()
}
