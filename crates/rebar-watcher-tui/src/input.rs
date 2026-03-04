use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::app::{App, View};

pub fn handle_key(app: &mut App, key: KeyEvent, item_count: usize) {
    match key.code {
        KeyCode::Char('q') | KeyCode::Esc => app.should_quit = true,
        KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            app.should_quit = true;
        }

        KeyCode::Char('1') => {
            app.view = View::Tree;
            app.scroll_offset = 0;
        }
        KeyCode::Char('2') => {
            app.view = View::List;
            app.scroll_offset = 0;
        }
        KeyCode::Char('3') => {
            app.view = View::Events;
            app.scroll_offset = 0;
        }

        KeyCode::Up | KeyCode::Char('k') => app.scroll_up(),
        KeyCode::Down | KeyCode::Char('j') => app.scroll_down(item_count),

        KeyCode::Char('s') => {
            app.sort_column = app.sort_column.next();
        }

        _ => {}
    }
}
