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

/// Multi-node mode: one sampler per node (local + remotes), the ALL view
/// aggregates them, and Enter drills into a single node's familiar UI.
pub fn run_multi(spec: Option<&str>) -> io::Result<()> {
    crossterm::terminal::enable_raw_mode()?;
    let mut stdout = io::stdout();
    crossterm::execute!(stdout, crossterm::terminal::EnterAlternateScreen)?;
    glyphs::detect();

    let backend = ratatui::backend::CrosstermBackend::new(stdout);
    let mut terminal = ratatui::Terminal::new(backend)?;
    let mut app = app::App::new_multi();
    let mut manager = crate::nodes::NodeManager::spawn(spec, app.refresh_interval, None)?;
    // Register the manager's remote nodes in the app (index 0 is local).
    for node in manager.nodes.iter().skip(1) {
        app.add_node(&node.name);
    }
    // Previous link state per node: a transition into Live means a fresh
    // handshake (fresh MonoMap anchor), so the node's rate baselines must
    // re-establish or the first post-reconnect frame computes garbage rates.
    let mut prev_links = vec![crate::remote::LinkState::Connecting; manager.nodes.len()];

    loop {
        manager.refresh_names();
        for (idx, node) in manager.nodes.iter().enumerate() {
            // Only the local node has a sampler; a remote node's data must
            // come exclusively from its RemoteSampler or it would display
            // the viewer's own hardware.
            if let Some(sampler) = &node.sampler {
                if let Some(snap) = sampler.try_latest() {
                    app.apply_node_snapshot(idx, snap);
                }
            }
            if let Some(remote) = &node.remote {
                if let Some(snap) = remote.try_latest() {
                    app.apply_node_snapshot(idx, snap);
                }
                let state = remote.state();
                if state == crate::remote::LinkState::Live
                    && prev_links[idx] != crate::remote::LinkState::Live
                {
                    app.reset_node_baseline(idx);
                }
                prev_links[idx] = state;
                app.set_node_state(idx, state);
            }
        }

        terminal.draw(|frame| ui::draw(frame, &mut app))?;
        events::handle_events(&mut app)?;
        // `<`/`>` change app.refresh_interval; mirror it to every sampler.
        for node in &manager.nodes {
            if let Some(sampler) = &node.sampler {
                sampler.set_interval(app.refresh_interval);
            }
            if let Some(remote) = &node.remote {
                remote.set_interval(app.refresh_interval);
            }
        }

        if app.should_quit {
            break;
        }
    }

    manager.stop_all();
    crossterm::terminal::disable_raw_mode()?;
    crossterm::execute!(
        terminal.backend_mut(),
        crossterm::terminal::LeaveAlternateScreen
    )?;
    Ok(())
}
