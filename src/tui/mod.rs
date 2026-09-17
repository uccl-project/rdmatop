pub mod app;
pub mod events;
pub mod glyphs;
pub mod theme;
pub mod ui;

use std::io;

use crate::sampler;

pub fn run() -> io::Result<()> {
    crossterm::terminal::enable_raw_mode()?;
    let mut stdout = io::stdout();
    crossterm::execute!(stdout, crossterm::terminal::EnterAlternateScreen)?;
    glyphs::detect();

    let backend = ratatui::backend::CrosstermBackend::new(stdout);
    let mut terminal = ratatui::Terminal::new(backend)?;
    let mut app = app::App::new();
    let sampler = sampler::Sampler::spawn(app.refresh_interval);

    loop {
        if let Some(snap) = sampler.try_latest() {
            app.apply_snapshot(snap);
        }
        if app.sampler_error.is_none() {
            app.sampler_error = sampler.death_reason();
        }

        terminal.draw(|frame| ui::draw(frame, &mut app))?;
        events::handle_events(&mut app)?;
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
