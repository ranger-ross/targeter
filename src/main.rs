use std::{
    path::{Path, PathBuf},
    sync::mpsc,
    time::Duration,
};

use crossterm::{
    event::{self, Event},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use eyre::{Context, Result};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;

use app::App;
use args::Args;
use poll::Poller;
use scan::{TargetEntry, resolve_root};
use ui::input::{Action, handle_key};

mod app;
mod args;
mod poll;
mod scan;
mod trace;
mod ui;

fn main() -> Result<()> {
    let _trace_guard = trace::init();
    let Args { root } = Args::new();
    if !root.is_dir() {
        eyre::bail!("scan root is not a directory: {}", root.display());
    }
    let root = resolve_root(&root);

    enable_raw_mode().wrap_err("enabling terminal raw mode")?;
    let mut stdout = std::io::stdout();
    execute!(stdout, EnterAlternateScreen).wrap_err("entering alternate screen")?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend).wrap_err("creating terminal")?;

    let result = run(&mut terminal, root);

    disable_raw_mode().wrap_err("disabling terminal raw mode")?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen).wrap_err("leaving alternate screen")?;
    terminal.show_cursor().wrap_err("restoring cursor")?;
    if let Some(guard) = _trace_guard.as_ref() {
        eprintln!("Trace written to {}", guard.path.display());
    }

    result
}

fn run(terminal: &mut Terminal<CrosstermBackend<std::io::Stdout>>, root: PathBuf) -> Result<()> {
    let mut app = App::new(root.clone());
    let mut scan_rx = spawn_scan(&root);
    let mut poller = Poller::new();

    loop {
        let _frame = tracing::info_span!("frame").entered();
        terminal
            .draw(|frame| ui::render(frame, &mut app))
            .wrap_err("rendering frame")?;

        // Pick up a finished background scan without blocking the UI.
        if let Some(rx) = scan_rx.as_ref()
            && let Ok((entries, build_cache)) = rx.try_recv()
        {
            app.set_entries(entries, build_cache);
            scan_rx = None;
            poller.reset(&app);
        }

        poller.poll(&mut app);
        // 60fps while loading, 10fps otherwise.
        let frame_budget = if app.scanning {
            Duration::from_millis(16)
        } else {
            Duration::from_millis(100)
        };
        if event::poll(frame_budget).wrap_err("polling terminal events")? {
            match event::read().wrap_err("reading terminal event")? {
                Event::Key(key) => match handle_key(&mut app, key) {
                    Action::Continue => {}
                    Action::Quit => return Ok(()),
                    Action::Rescan => {
                        app.begin_scan();
                        scan_rx = spawn_scan(&app.root);
                    }
                },
                Event::Resize(_, _) => {}
                _ => {}
            }
        }
    }
}

/// Run the recursive scan off the UI thread so the TUI stays responsive.
/// The build-cache measurement rides along so the UI gets both at once.
fn spawn_scan(root: &Path) -> Option<mpsc::Receiver<(Vec<TargetEntry>, Option<TargetEntry>)>> {
    let root = root.to_path_buf();
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let entries = scan::scan(&root);
        let build_cache = scan::build_cache_entry();
        let _ = tx.send((entries, build_cache));
    });
    Some(rx)
}
