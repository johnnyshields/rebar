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

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers};

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent {
            code,
            modifiers: KeyModifiers::NONE,
            kind: KeyEventKind::Press,
            state: KeyEventState::NONE,
        }
    }

    fn key_with_mod(code: KeyCode, modifiers: KeyModifiers) -> KeyEvent {
        KeyEvent {
            code,
            modifiers,
            kind: KeyEventKind::Press,
            state: KeyEventState::NONE,
        }
    }

    #[test]
    fn quit_on_q() {
        let mut app = App::new();
        handle_key(&mut app, key(KeyCode::Char('q')), 10);
        assert!(app.should_quit);
    }

    #[test]
    fn quit_on_esc() {
        let mut app = App::new();
        handle_key(&mut app, key(KeyCode::Esc), 10);
        assert!(app.should_quit);
    }

    #[test]
    fn quit_on_ctrl_c() {
        let mut app = App::new();
        handle_key(&mut app, key_with_mod(KeyCode::Char('c'), KeyModifiers::CONTROL), 10);
        assert!(app.should_quit);
    }

    #[test]
    fn switch_to_tree_view() {
        let mut app = App::new();
        app.view = View::List;
        handle_key(&mut app, key(KeyCode::Char('1')), 10);
        assert!(matches!(app.view, View::Tree));
        assert_eq!(app.scroll_offset, 0);
    }

    #[test]
    fn switch_to_list_view() {
        let mut app = App::new();
        handle_key(&mut app, key(KeyCode::Char('2')), 10);
        assert!(matches!(app.view, View::List));
    }

    #[test]
    fn switch_to_events_view() {
        let mut app = App::new();
        handle_key(&mut app, key(KeyCode::Char('3')), 10);
        assert!(matches!(app.view, View::Events));
    }

    #[test]
    fn scroll_up_with_k() {
        let mut app = App::new();
        app.scroll_offset = 5;
        handle_key(&mut app, key(KeyCode::Char('k')), 10);
        assert_eq!(app.scroll_offset, 4);
    }

    #[test]
    fn scroll_down_with_j() {
        let mut app = App::new();
        handle_key(&mut app, key(KeyCode::Char('j')), 10);
        assert_eq!(app.scroll_offset, 1);
    }

    #[test]
    fn cycle_sort_column() {
        let mut app = App::new();
        assert_eq!(app.sort_column.label(), "PID");
        handle_key(&mut app, key(KeyCode::Char('s')), 10);
        assert_eq!(app.sort_column.label(), "Name");
    }
}
