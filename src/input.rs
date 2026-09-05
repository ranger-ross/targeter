use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::app::App;

/// What the event loop should do after a key press.
pub enum Action {
    Continue,
    Quit,
    Rescan,
}

/// Route a key press to the app. Filtering mode consumes edits.
pub fn handle_key(app: &mut App, key: KeyEvent) -> Action {
    // Ctrl+C always quits, even mid-filter.
    if key.code == KeyCode::Char('c') && key.modifiers == KeyModifiers::CONTROL {
        return Action::Quit;
    }
    if app.filtering {
        match key.code {
            KeyCode::Enter => app.filtering = false,
            KeyCode::Esc => app.filtering = false,
            KeyCode::Backspace => {
                app.filter_text.pop();
                let text = app.filter_text.clone();
                app.set_filter(text);
            }
            KeyCode::Char('u') if key.modifiers == KeyModifiers::CONTROL => {
                app.set_filter(String::new());
            }
            KeyCode::Char(c) => {
                app.filter_text.push(c);
                let text = app.filter_text.clone();
                app.set_filter(text);
            }
            _ => {}
        }
        return Action::Continue;
    }
    match (key.code, key.modifiers) {
        (KeyCode::Char('q'), _) | (KeyCode::Esc, _) => Action::Quit,
        (KeyCode::Char('/'), _) => {
            app.filtering = true;
            Action::Continue
        }
        (KeyCode::Down, _) | (KeyCode::Char('j'), _) => {
            app.next();
            Action::Continue
        }
        (KeyCode::Up, _) | (KeyCode::Char('k'), _) => {
            app.previous();
            Action::Continue
        }
        (KeyCode::Char('g'), _) => {
            app.top();
            Action::Continue
        }
        (KeyCode::Char('G'), _) => {
            app.bottom();
            Action::Continue
        }
        (KeyCode::Char('s'), _) => {
            app.cycle_sort();
            Action::Continue
        }
        (KeyCode::Char('r'), _) => Action::Rescan,
        _ => Action::Continue,
    }
}
