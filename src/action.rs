use crossterm::event::KeyEvent;

use crate::event::TaskEvent;

/// Actions are the only input accepted by [`crate::app::App`]. Raw terminal
/// events and background task events are converted here before state changes.
#[derive(Clone, Debug)]
pub enum Action {
    Key(KeyEvent),
    Task(Box<TaskEvent>),
    Tick,
}
