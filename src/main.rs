mod net;
mod netlink;
mod rdma;
mod stat;
mod trace;
mod tui;

use std::io;
use std::time::Instant;

fn run_tui() -> io::Result<()> {
    crossterm::terminal::enable_raw_mode()?;
    let mut stdout = io::stdout();
    crossterm::execute!(stdout, crossterm::terminal::EnterAlternateScreen)?;

    let backend = ratatui::backend::CrosstermBackend::new(stdout);
    let mut terminal = ratatui::Terminal::new(backend)?;
    let mut app = tui::app::App::new();

    let mut last_refresh = Instant::now() - app.refresh_interval;

    loop {
        if last_refresh.elapsed() >= app.refresh_interval {
            app.refresh_stats();
            last_refresh = Instant::now();
        }

        terminal.draw(|frame| tui::ui::draw(frame, &mut app))?;
        tui::events::handle_events(&mut app)?;

        if app.should_quit {
            break;
        }
    }

    crossterm::terminal::disable_raw_mode()?;
    crossterm::execute!(
        terminal.backend_mut(),
        crossterm::terminal::LeaveAlternateScreen
    )?;
    Ok(())
}

fn main() -> io::Result<()> {
    run_tui()
}
