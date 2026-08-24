use std::io::{self, Stdout, stdout};

use crossterm::{
    event::{DisableBracketedPaste, EnableBracketedPaste},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::{Terminal, backend::CrosstermBackend};

use crate::app::App;

use super::ui;

pub struct TuiTerminal {
    terminal: Terminal<CrosstermBackend<Stdout>>,
}

impl TuiTerminal {
    pub fn enter() -> io::Result<Self> {
        enable_raw_mode()?;
        let mut output = stdout();
        if let Err(error) = execute!(output, EnterAlternateScreen, EnableBracketedPaste) {
            let _ = execute!(stdout(), DisableBracketedPaste, LeaveAlternateScreen);
            let _ = disable_raw_mode();
            return Err(error);
        }
        let backend = CrosstermBackend::new(output);
        match Terminal::new(backend) {
            Ok(terminal) => Ok(Self { terminal }),
            Err(error) => {
                let _ = execute!(stdout(), DisableBracketedPaste, LeaveAlternateScreen);
                let _ = disable_raw_mode();
                Err(error)
            }
        }
    }

    pub fn draw(&mut self, app: &App) -> io::Result<()> {
        self.terminal
            .draw(|frame| ui::render(frame, app))
            .map(|_| ())
    }
}

impl Drop for TuiTerminal {
    fn drop(&mut self) {
        let _ = execute!(
            self.terminal.backend_mut(),
            DisableBracketedPaste,
            LeaveAlternateScreen
        );
        let _ = disable_raw_mode();
        let _ = self.terminal.show_cursor();
    }
}
