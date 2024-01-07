#![feature(trait_alias)]

use crossterm::event::*;
use crossterm::terminal::*;
use crossterm::*;
use ratatui::prelude::*;
use ratatui::symbols::border;
use ratatui::widgets::block::title;
use ratatui::widgets::*;
use std::io::*;

const VERSION: &str = env!("CARGO_PKG_VERSION");

// Application state
#[derive(Default)]
pub struct App {
    counter: i64,
    should_quit: bool,
}

/// helper function to create a centered rect using up certain percentage of the available rect `r`
fn centered_rect(r: Rect, min_x: u16, min_y: u16) -> Rect {
    // Cut the given rectangle into three vertical pieces
    let popup_layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(0),
            Constraint::Percentage(min_y),
            Constraint::Min(0),
        ])
        .split(r);

    // Then cut the middle vertical piece into three width-wise pieces
    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Min(0),
            Constraint::Percentage(min_x),
            Constraint::Min(0),
        ])
        .split(popup_layout[1])[1] // Return the middle chunk
}

pub fn center_paragraph<T>(text: T) -> Paragraph<'static>
where
    T: Into<Text<'static>>,
{
    Paragraph::new(text).alignment(Alignment::Center)
}

fn main() -> Result<()> {
    // Alternate screen is a 2nd buffer that allows this app to
    // draw to the terminal without overwriting existing terminal output
    stdout().execute(EnterAlternateScreen)?;
    // Raw mode turns off I/O processing by the terminal,
    // giving this app control over when to print to the screen.
    enable_raw_mode()?;
    let mut terminal = Terminal::new(CrosstermBackend::new(stdout()))?;
    terminal.clear()?;

    let mut app = App {
        ..Default::default()
    };

    loop {
        // Draw TUI
        terminal.draw(|f| {
            let chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Length(3), Constraint::Min(1)])
                .split(f.size());

            let title_block = Block::default()
                .title_alignment(Alignment::Center)
                .borders(Borders::ALL);

            f.render_widget(
                center_paragraph(Text::styled(
                    format!("WinDedupe v{}", VERSION),
                    Style::default().fg(Color::Green),
                ))
                .block(title_block),
                chunks[0],
            );
            f.render_widget(
                center_paragraph(
                    format!("Counter: {}", app.counter),
                ),
                chunks[1],
            );
        })?;

        // Handle events every 100 ms
        if event::poll(std::time::Duration::from_millis(100))? {
            if let event::Event::Key(key) = event::read()? {
                if key.kind == KeyEventKind::Press {
                    match key.code {
                        KeyCode::Up => app.counter += 1,
                        KeyCode::Down => app.counter -= 1,
                        KeyCode::Char('q') => app.should_quit = true,
                        _ => {}
                    }
                }
            }
        }

        if app.should_quit {
            break;
        }
    }

    // Restore old terminal state
    stdout().execute(LeaveAlternateScreen)?;
    disable_raw_mode()?;
    Ok(())
}
