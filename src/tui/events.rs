use crossterm::event::{self, Event, KeyCode, KeyEventKind};
use std::time::Duration;

use super::app::App;

pub fn handle_events(app: &mut App) -> std::io::Result<bool> {
    if !event::poll(Duration::from_millis(200))? {
        return Ok(false);
    }
    if let Event::Key(key) = event::read()? {
        if key.kind != KeyEventKind::Press {
            return Ok(false);
        }
        // Window input popup takes priority
        if app.show_window_input {
            handle_window_input(app, key.code);
            return Ok(true);
        }
        // Column picker takes priority
        if app.show_column_picker {
            handle_column_picker(app, key.code);
            return Ok(true);
        }
        // Help toggle from any mode
        if key.code == KeyCode::Char('h') {
            app.show_help = !app.show_help;
            return Ok(true);
        }
        // Dismiss help with Esc
        if app.show_help {
            if key.code == KeyCode::Esc {
                app.show_help = false;
            }
            return Ok(true);
        }
        if app.show_detail {
            handle_detail_mode(app, key.code);
        } else {
            handle_normal_mode(app, key.code);
        }
        return Ok(true);
    }
    Ok(false)
}

fn handle_normal_mode(app: &mut App, key: KeyCode) {
    match key {
        KeyCode::Char('q') | KeyCode::Esc => app.should_quit = true,
        KeyCode::Up | KeyCode::Char('k') => app.move_up(),
        KeyCode::Down | KeyCode::Char('j') => app.move_down(),
        KeyCode::Left => app.scroll_left(),
        KeyCode::Right => app.scroll_right(),
        KeyCode::Enter => app.toggle_detail(),
        KeyCode::Char('t') => app.cycle_theme(),
        KeyCode::Char('a') => app.toggle_rolling_avg(),
        KeyCode::Char('+') | KeyCode::Char('=') => app.increase_avg_window(),
        KeyCode::Char('-') => app.decrease_avg_window(),
        KeyCode::Char('>') => app.increase_refresh_interval(),
        KeyCode::Char('<') => app.decrease_refresh_interval(),
        KeyCode::Char('w') => app.open_window_input(),
        KeyCode::Char('c') => app.open_column_picker(),
        KeyCode::Char('r') => app.toggle_recording(),
        _ => {}
    }
}

fn handle_detail_mode(app: &mut App, key: KeyCode) {
    match key {
        KeyCode::Char('q') => app.should_quit = true,
        KeyCode::Esc | KeyCode::Enter => app.toggle_detail(),
        KeyCode::Up | KeyCode::Char('k') => app.detail_scroll_up(),
        KeyCode::Down | KeyCode::Char('j') => app.detail_scroll_down(app.detail_max_scroll),
        KeyCode::Char('t') => app.cycle_theme(),
        KeyCode::Char('a') => app.toggle_rolling_avg(),
        KeyCode::Char('+') | KeyCode::Char('=') => app.increase_avg_window(),
        KeyCode::Char('-') => app.decrease_avg_window(),
        KeyCode::Char('>') => app.increase_refresh_interval(),
        KeyCode::Char('<') => app.decrease_refresh_interval(),
        KeyCode::Char('w') => app.open_window_input(),
        KeyCode::Char('c') => app.open_column_picker(),
        KeyCode::Char('r') => app.toggle_recording(),
        _ => {}
    }
}

fn handle_window_input(app: &mut App, key: KeyCode) {
    match key {
        KeyCode::Esc => app.cancel_window_input(),
        KeyCode::Enter => app.confirm_window_input(),
        KeyCode::Backspace => {
            app.window_input_buf.pop();
        }
        KeyCode::Char(c) if c.is_ascii_digit() && app.window_input_buf.len() < 4 => {
            app.window_input_buf.push(c);
        }
        _ => {}
    }
}

fn handle_column_picker(app: &mut App, key: KeyCode) {
    match key {
        KeyCode::Esc | KeyCode::Char('c') => app.close_column_picker(),
        KeyCode::Up | KeyCode::Char('k') => app.column_picker_up(),
        KeyCode::Down | KeyCode::Char('j') => app.column_picker_down(),
        KeyCode::Char(' ') | KeyCode::Enter => app.column_picker_toggle(),
        _ => {}
    }
}
