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

use app::{probe_latency, probe_target, App, ModelProbeResult};

/// Run the interactive TUI for a given tool.
///
/// Keys (list mode): j/k or arrows to move, Enter to switch, `t` to probe the
/// selected provider's latency, `a`/`e`/`d` to add/edit/remove it, `g` for
/// global defaults, Tab to cycle tools, q or Ctrl+C to quit.
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
        let _ = execute!(io::stdout(), LeaveAlternateScreen, crossterm::cursor::Show);
        original_hook(info);
    }));

    let backend = CrosstermBackend::new(stdout);
    let mut terminal = match Terminal::new(backend) {
        Ok(t) => t,
        Err(e) => {
            let _ = disable_raw_mode();
            let _ = execute!(io::stdout(), LeaveAlternateScreen, crossterm::cursor::Show);
            let _ = std::panic::take_hook();
            return Err(CoreError::Proxy(format!("init terminal: {e}")));
        }
    };

    let result = event_loop(core, tool, &mut terminal);

    // ── Terminal cleanup ──────────────────────────────────────────────
    // Ensure all restoration steps execute unconditionally even if one fails.
    let _ = disable_raw_mode();
    let _ = execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        crossterm::cursor::Show
    );
    let _ = terminal.show_cursor();
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
    let (probe_tx, probe_rx) = mpsc::channel::<(ToolKind, String, Option<u128>)>();
    // Accumulators for the end-of-round summary message.
    let (mut probe_ok, mut probe_fail, mut probe_worst) = (0u32, 0u32, 0u128);

    // `/v1/models` catalog probes run in worker threads; results arrive here.
    let (model_probe_tx, model_probe_rx) =
        mpsc::channel::<(ToolKind, String, std::result::Result<Vec<String>, String>)>();
    let (mut mprobe_ok, mut mprobe_fail, mut mprobe_total) = (0u32, 0u32, 0usize);

    // Redraw only when state changed or an animation is in flight, so an idle
    // terminal doesn't repaint at the poll cadence.
    let mut dirty = true;
    loop {
        let mut drained = false;
        while let Ok((t, id, ms)) = probe_rx.try_recv() {
            if t != app.tool {
                continue;
            }
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

        let mut model_drained = false;
        while let Ok((t, id, res)) = model_probe_rx.try_recv() {
            if t != app.tool {
                continue;
            }
            app.model_probing.remove(&id);
            match res {
                Ok(models) => {
                    mprobe_ok += 1;
                    mprobe_total += models.len();
                    app.model_probes
                        .insert(id, ModelProbeResult::Models(models));
                }
                Err(err) => {
                    mprobe_fail += 1;
                    app.model_probes.insert(id, ModelProbeResult::Failed(err));
                }
            }
            model_drained = true;
        }
        if model_drained {
            dirty = true;
            if app.model_probing.is_empty() {
                let n = mprobe_ok + mprobe_fail;
                app.message = Some(if mprobe_fail == 0 {
                    format!("models: {mprobe_ok}/{n} ok · {mprobe_total} models")
                } else if mprobe_ok == 0 {
                    format!("models: all {n} failed")
                } else {
                    format!(
                        "models: {mprobe_ok}/{n} ok · {mprobe_fail} failed · {mprobe_total} models"
                    )
                });
                mprobe_ok = 0;
                mprobe_fail = 0;
                mprobe_total = 0;
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

        if app.delete_confirm.is_some() {
            match key.code {
                KeyCode::Char('y') | KeyCode::Char('Y') => app.confirm_delete(core, true),
                // Enter takes the [y/N] default, which is No.
                KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc | KeyCode::Enter => {
                    app.confirm_delete(core, false)
                }
                _ => {}
            }
            continue;
        }

        if app.is_model_mapping() {
            let is_editing = app
                .model_map
                .as_ref()
                .is_some_and(|mm| mm.editing.is_some());
            if is_editing {
                match key.code {
                    KeyCode::Esc => app.cancel_edit(),
                    KeyCode::Enter => app.submit_input(core),
                    KeyCode::Tab | KeyCode::Down => app.edit_next_field(),
                    KeyCode::BackTab | KeyCode::Up => app.edit_prev_field(),
                    KeyCode::Left => app.edit_cycle(false),
                    KeyCode::Right => app.edit_cycle(true),
                    KeyCode::Backspace => app.edit_backspace(),
                    KeyCode::Char(c) => app.edit_char(c),
                    _ => {}
                }
            } else {
                match key.code {
                    KeyCode::Esc | KeyCode::Char('q') => app.cancel_edit(),
                    KeyCode::Char('j') | KeyCode::Down => app.model_map_next(),
                    KeyCode::Char('k') | KeyCode::Up => app.model_map_prev(),
                    KeyCode::Char('a') => app.model_map_start_add(),
                    KeyCode::Char('e') | KeyCode::Enter => app.model_map_start_edit(),
                    KeyCode::Char('d') => app.model_map_delete(core),
                    KeyCode::Char(' ') => app.model_map_toggle_merge(core),
                    _ => {}
                }
            }
            continue;
        }

        if app.is_routing() {
            let editing_model = app.routes_edit.as_ref().is_some_and(|r| r.editing_model);
            if editing_model {
                match key.code {
                    KeyCode::Esc | KeyCode::Enter => app.routes_finish_edit_model(),
                    KeyCode::Left => app.edit_cycle(false),
                    KeyCode::Right => app.edit_cycle(true),
                    KeyCode::Backspace => app.edit_backspace(),
                    KeyCode::Char(c) => app.edit_char(c),
                    _ => {}
                }
            } else {
                match key.code {
                    KeyCode::Esc | KeyCode::Char('q') => app.cancel_edit(),
                    KeyCode::Enter => app.submit_input(core),
                    KeyCode::Char('j') | KeyCode::Down => app.routes_next(),
                    KeyCode::Char('k') | KeyCode::Up => app.routes_prev(),
                    KeyCode::Char(' ') => app.routes_toggle_selected(),
                    KeyCode::Char('K') | KeyCode::Char('+') => app.routes_move_up(),
                    KeyCode::Char('J') | KeyCode::Char('-') => app.routes_move_down(),
                    KeyCode::Char('e') | KeyCode::Char('m') => app.routes_start_edit_model(),
                    _ => {}
                }
            }
            continue;
        }

        if app.is_input_mode() {
            match key.code {
                KeyCode::Esc => app.cancel_edit(),
                KeyCode::Enter => {
                    app.submit_input(core);
                    stats = load_stats(core, &app.tool);
                }
                KeyCode::Tab | KeyCode::Down => app.edit_next_field(),
                KeyCode::BackTab | KeyCode::Up => app.edit_prev_field(),
                KeyCode::Left => app.edit_cycle(false),
                KeyCode::Right => app.edit_cycle(true),
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
            KeyCode::BackTab => {
                app.prev_tool(core);
                stats = load_stats(core, &app.tool);
            }
            KeyCode::Enter | KeyCode::Char(' ') => {
                app.switch_selected(core);
                stats = load_stats(core, &app.tool);
            }
            KeyCode::Char('e') => app.start_edit(),
            KeyCode::Char('a') => app.start_add(core),
            KeyCode::Char('g') => app.start_config(core),
            KeyCode::Char('m') => app.start_model_map(core),
            KeyCode::Char('r') => app.start_routes(core),
            KeyCode::Char('d') => app.request_delete(),
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
                        let current_tool = app.tool;
                        for (id, host, port) in targets {
                            let tx = probe_tx.clone();
                            app.probing.insert(id.clone());
                            std::thread::spawn(move || {
                                let ms = probe_latency(&host, port, Duration::from_secs(3));
                                let _ = tx.send((current_tool, id, ms));
                            });
                        }
                        app.message = Some(format!("probing {n} providers…"));
                    }
                }
            }
            KeyCode::Char('p') => {
                if !app.model_probing.is_empty() {
                    app.message = Some("model probe already running…".into());
                } else {
                    let mut targets = Vec::new();
                    for p in &app.providers {
                        if let Ok((base_url, protocol, auth_key)) = core.resolve_probe_params(p) {
                            targets.push((p.id.clone(), base_url, protocol, auth_key));
                        }
                    }
                    if targets.is_empty() {
                        app.message = Some("no probeable provider for /v1/models".into());
                    } else {
                        let n = targets.len();
                        let current_tool = app.tool;
                        for (id, base_url, protocol, auth_key) in targets {
                            let tx = model_probe_tx.clone();
                            app.model_probing.insert(id.clone());
                            std::thread::spawn(move || {
                                let res = match tokio::runtime::Builder::new_current_thread()
                                    .enable_all()
                                    .build()
                                {
                                    Ok(rt) => rt.block_on(xfade_core::service::probe_models_http(
                                        &base_url,
                                        protocol,
                                        auth_key.as_deref(),
                                        Duration::from_secs(5),
                                    )),
                                    Err(e) => Err(format!("runtime: {e}")),
                                };
                                let _ = tx.send((current_tool, id, res));
                            });
                        }
                        app.message = Some(format!("probing models on {n} providers…"));
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
