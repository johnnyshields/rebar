mod app;
mod input;
mod views;

use std::error::Error;
use std::io;
use std::sync::Arc;
use std::time::Duration;

use crossterm::event::{self, Event};
use crossterm::terminal::{self, EnterAlternateScreen, LeaveAlternateScreen};
use crossterm::ExecutableCommand;
use ratatui::prelude::*;
use ratatui::widgets::{Block, Borders, Tabs};

use rebar_core::runtime::Runtime;

use crate::app::{App, View};

/// Configuration for the TUI watcher.
pub struct WatcherTuiConfig {
    pub tick_rate_ms: u64,
}

impl Default for WatcherTuiConfig {
    fn default() -> Self {
        Self { tick_rate_ms: 250 }
    }
}

/// Run the TUI watcher. Blocks until the user quits.
pub async fn run(runtime: &Runtime, config: WatcherTuiConfig) -> Result<(), Box<dyn Error>> {
    let tick_rate = Duration::from_millis(config.tick_rate_ms);

    // Set up event bus subscriber
    let mut event_rx = runtime
        .event_bus()
        .ok_or("runtime has no event bus attached")?
        .subscribe();

    let table = Arc::clone(runtime.table());

    // Enter raw mode + alternate screen
    terminal::enable_raw_mode()?;
    io::stdout().execute(EnterAlternateScreen)?;

    let backend = CrosstermBackend::new(io::stdout());
    let mut terminal = Terminal::new(backend)?;

    let mut app = App::new();

    loop {
        // Drain broadcast events
        while let Ok(lifecycle_event) = event_rx.try_recv() {
            app.push_event(lifecycle_event);
        }

        // Refresh process snapshot
        app.processes = table.snapshot();
        app.sort_processes();

        let item_count = match app.view {
            View::Tree => app.processes.len(),
            View::List => app.processes.len(),
            View::Events => app.events.len(),
        };

        // Draw
        terminal.draw(|frame| {
            let chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Length(3), Constraint::Min(0)])
                .split(frame.area());

            // Tab bar
            let tab_index = match app.view {
                View::Tree => 0,
                View::List => 1,
                View::Events => 2,
            };
            let tabs = Tabs::new(vec!["1:Tree", "2:List", "3:Events"])
                .select(tab_index)
                .block(
                    Block::default()
                        .title(format!(
                            " Rebar Watcher | {} processes ",
                            app.processes.len()
                        ))
                        .borders(Borders::ALL)
                        .border_style(Style::default().fg(Color::Blue)),
                )
                .highlight_style(Style::default().fg(Color::Yellow).bold());

            frame.render_widget(tabs, chunks[0]);

            // Content
            match app.view {
                View::Tree => views::tree::render(frame, chunks[1], &app),
                View::List => views::list::render(frame, chunks[1], &app),
                View::Events => views::events::render(frame, chunks[1], &app),
            }
        })?;

        if app.should_quit {
            break;
        }

        // Poll for input
        if event::poll(tick_rate)? {
            if let Event::Key(key) = event::read()? {
                input::handle_key(&mut app, key, item_count);
            }
        }
    }

    // Restore terminal
    terminal::disable_raw_mode()?;
    io::stdout().execute(LeaveAlternateScreen)?;

    Ok(())
}
