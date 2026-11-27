//! Native 60-FPS Rust Dual-Panel TUI entrypoint (`kgsh-tui`) for kaggle-shell-mcp.

mod app;
mod bridge;
mod file_icons;
mod state;
mod ui;
mod worker;

use anyhow::Result;
use clap::Parser;
use crossterm::event::{
    DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture, Event,
    EventStream, KeyEventKind,
};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use futures::StreamExt;
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;
use std::io::stdout;
use std::path::PathBuf;
use std::time::{Duration, Instant};
use tokio::sync::mpsc;

use crate::app::App;
use crate::bridge::PythonBridge;
use crate::state::resolve_state_dir;

#[derive(Parser, Debug)]
#[command(name = "kgsh-tui", version = "1.0.1", about = "Native Rust 60-FPS Dual-Panel TUI Cockpit for kaggle-shell-mcp")]
struct Cli {
    /// Initial worker slot to focus
    #[arg(short, long, default_value = "1")]
    slot: String,

    /// Optional override for STATE_DIR (~/.kaggle_mcp or repo .kaggle_mcp)
    #[arg(long)]
    state_dir: Option<PathBuf>,

    /// Optional explicit Python executable for kaggle_mcp.bridge
    #[arg(long)]
    python: Option<String>,
}

fn restore_terminal() {
    let _ = disable_raw_mode();
    let _ = execute!(
        stdout(),
        LeaveAlternateScreen,
        DisableMouseCapture,
        DisableBracketedPaste
    );
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let state_dir = resolve_state_dir(cli.state_dir);
    let bridge = PythonBridge::discover(cli.python);

    let orig_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        restore_terminal();
        orig_hook(info);
    }));

    enable_raw_mode()?;
    let mut out = stdout();
    execute!(
        out,
        EnterAlternateScreen,
        EnableMouseCapture,
        EnableBracketedPaste
    )?;
    let backend = CrosstermBackend::new(out);
    let mut terminal = Terminal::new(backend)?;
    terminal.clear()?;

    let (tx, mut rx) = mpsc::unbounded_channel();
    let mut app = App::new(state_dir, cli.slot, bridge, tx);
    app.spawn_prewarm_all();
    app.spawn_quota();
    app.poll_tick();

    let mut events = EventStream::new();
    let mut anim_tick = tokio::time::interval(Duration::from_millis(33)); // ~30 FPS when telemetry/pulse active
    let mut bus_tick = tokio::time::interval(Duration::from_millis(250));
    let mut poll_tick = tokio::time::interval(Duration::from_millis(450));
    let mut quota_tick = tokio::time::interval(Duration::from_secs(60));

    let mut needs_redraw = true;

    loop {
        if needs_redraw {
            terminal.draw(|f| {
                ui::render(f, &mut app);
                app.capture_screen_snapshot(f.buffer_mut());
                ui::render_selection_overlay(f, &app);
            })?;
            needs_redraw = false;
        }

        if app.should_quit {
            break;
        }

        tokio::select! {
            Some(Ok(ev)) = events.next() => {
                match ev {
                    Event::Key(k) => {
                        if k.kind == KeyEventKind::Release {
                            continue;
                        }
                        app.handle_key(k);
                        if app.mouse_capture_changed {
                            app.mouse_capture_changed = false;
                            if app.mouse_capture {
                                let _ = execute!(stdout(), EnableMouseCapture);
                            } else {
                                let _ = execute!(stdout(), DisableMouseCapture);
                            }
                        }
                        needs_redraw = true;
                    }
                    Event::Mouse(m) => {
                        app.handle_mouse(m);
                        if app.mouse_capture_changed {
                            app.mouse_capture_changed = false;
                            if app.mouse_capture {
                                let _ = execute!(stdout(), EnableMouseCapture);
                            } else {
                                let _ = execute!(stdout(), DisableMouseCapture);
                            }
                        }
                        needs_redraw = true;
                    }
                    Event::Paste(pasted) => {
                        app.handle_paste(pasted);
                        needs_redraw = true;
                    }
                    Event::Resize(_, _) => {
                        needs_redraw = true;
                    }
                    _ => {}
                }
            }
            Some(msg) = rx.recv() => {
                app.handle_msg(msg);
                while let Ok(more) = rx.try_recv() {
                    app.handle_msg(more);
                }
                needs_redraw = true;
            }
            _ = anim_tick.tick() => {
                app.frame_tick = app.frame_tick.wrapping_add(1);
                app.conduit.step();
                if app.tick_pending_preview() {
                    needs_redraw = true;
                }
                if let Some(ref toast) = app.toast {
                    if Instant::now() >= toast.expires_at {
                        app.toast = None;
                        needs_redraw = true;
                    }
                }
                // Only redraw on animation tick when telemetry/plasma pulse/tasks or running exec are active
                if app.modal.is_none() && (app.show_telemetry || app.conduit.state_kind != "idle" || !app.active_tasks.is_empty() || app.running_exec.is_some()) {
                    needs_redraw = true;
                }
            }
            _ = bus_tick.tick() => {
                app.poll_local_flow_bus();
            }
            _ = poll_tick.tick() => {
                if app.should_poll_now() {
                    app.poll_tick();
                }
            }
            _ = quota_tick.tick() => {
                app.spawn_quota();
            }
        }
    }

    restore_terminal();
    Ok(())
}
