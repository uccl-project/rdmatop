mod net;
mod netlink;
mod nvlink;
mod rdma;
mod sampler;
mod stat;
mod trace;
mod tui;
mod xgmi;

use std::io;

fn run_tui() -> io::Result<()> {
    crossterm::terminal::enable_raw_mode()?;
    let mut stdout = io::stdout();
    crossterm::execute!(stdout, crossterm::terminal::EnterAlternateScreen)?;
    tui::glyphs::detect();

    let backend = ratatui::backend::CrosstermBackend::new(stdout);
    let mut terminal = ratatui::Terminal::new(backend)?;
    let mut app = tui::app::App::new();
    let sampler = sampler::Sampler::spawn(app.refresh_interval);

    loop {
        if let Some(snap) = sampler.try_latest() {
            app.apply_snapshot(snap);
        }
        if app.sampler_error.is_none() {
            app.sampler_error = sampler.death_reason();
        }

        terminal.draw(|frame| tui::ui::draw(frame, &mut app))?;
        tui::events::handle_events(&mut app)?;
        // `<`/`>` change app.refresh_interval; mirror it to the thread.
        sampler.set_interval(app.refresh_interval);

        if app.should_quit {
            break;
        }
    }

    sampler.stop();
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
