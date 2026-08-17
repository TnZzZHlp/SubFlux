use std::{thread, time::Duration};

use crossterm::event::{self, Event, KeyEventKind};
use tokio::sync::mpsc::UnboundedSender;

use crate::action::Action;

/// Crossterm blocks while polling stdin, so it lives in a tiny dedicated
/// thread. It only converts terminal input into Actions; all state changes
/// still happen in the Tokio-owned application loop.
pub fn spawn_input(sender: UnboundedSender<Action>) {
    thread::spawn(move || {
        loop {
            match event::poll(Duration::from_millis(100)) {
                Ok(true) => match event::read() {
                    Ok(Event::Key(key)) if key.kind != KeyEventKind::Release => {
                        if sender.send(Action::Key(key)).is_err() {
                            break;
                        }
                    }
                    Ok(_) => {}
                    Err(_) => break,
                },
                Ok(false) => {}
                Err(_) => break,
            }
        }
    });
}
