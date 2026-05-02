use std::io::{self, Stdout};
use ratatui::{
    Terminal,
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph},
};
use crossterm::{
    execute,
    terminal::{enable_raw_mode, disable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use common::map::{Map, Tile};
use common::protocol::GameSnapshot;
use common::types::{Bomb, Explosion};

// A type alias for our specific terminal backend
// CrosstermBackend<Stdout> means: use crossterm, write to stdout
pub type Term = Terminal<CrosstermBackend<Stdout>>;

// Sets up the terminal for TUI rendering:
// - raw mode: keypresses go straight to our app, not the shell
// - alternate screen: like vim's buffer, restores original terminal on exit
pub fn setup() -> io::Result<Term> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    Terminal::new(backend)
}

// Must be called before the program exits — restores the terminal to normal
pub fn teardown(terminal: &mut Term) -> io::Result<()> {
    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;
    Ok(())
}

// Converts a map tile + context into a styled Span (a piece of text with color)
// A Span is ratatui's atomic unit of styled text
fn tile_span(map: &Map, x: u16, y: u16, snapshot: &GameSnapshot) -> Vec<Span<'static>> {
    // Check if there's a bomb at this position
    let has_bomb = snapshot.bombs.iter().any(|b| b.pos == (x, y));

    // Check if there's an explosion at this position
    let has_explosion = snapshot.explosions.iter()
        .any(|e| e.cells.contains(&(x, y)));

    // Check if a player is standing here
    let player_here = snapshot.players.iter()
        .find(|p| p.alive && p.pos == (x, y));

    // Each tile is rendered as 2 characters wide
    // This compensates for terminal cells being ~2:1 height:width ratio
    // Without this the map looks squished horizontally
    if let Some(player) = player_here {
        let color = match player.id {
            0 => Color::Cyan,
            1 => Color::Magenta,
            2 => Color::Yellow,
            _ => Color::Green,
        };
        let label = format!("{} ", player.id);
        return vec![Span::styled(label, Style::default().fg(color).add_modifier(Modifier::BOLD))];
    }

    if has_explosion {
        return vec![Span::styled("✸ ".to_string(), Style::default().fg(Color::Red).add_modifier(Modifier::BOLD))];
    }

    if has_bomb {
        return vec![Span::styled("● ".to_string(), Style::default().fg(Color::Yellow))];
    }

    match map.get(x, y) {
        Some(Tile::Wall)         => vec![Span::styled("██".to_string(), Style::default().fg(Color::DarkGray))],
        Some(Tile::Destructible) => vec![Span::styled("▒▒".to_string(), Style::default().fg(Color::Yellow))],
        Some(Tile::Empty)        => vec![Span::styled("  ".to_string(), Style::default())],
        None                     => vec![Span::styled("??".to_string(), Style::default().fg(Color::Red))],
    }
}

// Builds the map as a Paragraph widget
// We construct it row by row, each row being a Line of Spans
fn build_map_widget(map: &Map, snapshot: &GameSnapshot) -> Paragraph<'static> {
    let mut lines: Vec<Line> = Vec::new();

    for y in 0..map.height {
        let mut spans: Vec<Span> = Vec::new();
        for x in 0..map.width {
            spans.extend(tile_span(map, x, y, snapshot));
        }
        lines.push(Line::from(spans));
    }

    Paragraph::new(lines)
        .block(Block::default().borders(Borders::ALL).title(" BomberTerm "))
}

// Builds the sidebar showing player info
fn build_sidebar_widget(snapshot: &GameSnapshot) -> Paragraph<'static> {
    let mut lines: Vec<Line> = vec![
        Line::from(Span::styled("PLAYERS", Style::default().add_modifier(Modifier::BOLD))),
        Line::from(""),
    ];

    for player in &snapshot.players {
        let color = match player.id {
            0 => Color::Cyan,
            1 => Color::Magenta,
            2 => Color::Yellow,
            _ => Color::Green,
        };

        let status = if player.alive { "♥" } else { "✝" };
        let label = format!("{} {} {}", player.id, status, player.name);
        lines.push(Line::from(Span::styled(label, Style::default().fg(color))));
        let info = format!("  bombs: {}/{}", player.bombs_placed, player.max_bombs);
        lines.push(Line::from(Span::styled(info, Style::default().fg(Color::Gray))));
        lines.push(Line::from(""));
    }

    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        format!("Tick: {}", snapshot.tick),
        Style::default().fg(Color::DarkGray),
    )));

    Paragraph::new(lines)
        .block(Block::default().borders(Borders::ALL).title(" Info "))
}

// Builds the help bar at the bottom
fn build_help_widget() -> Paragraph<'static> {
    Paragraph::new(Line::from(vec![
        Span::styled(" ↑↓←→ ", Style::default().fg(Color::Yellow)),
        Span::raw("Move  "),
        Span::styled(" Space ", Style::default().fg(Color::Yellow)),
        Span::raw("Bomb  "),
        Span::styled(" Q ", Style::default().fg(Color::Yellow)),
        Span::raw("Quit"),
    ]))
}

// The main render function — called every frame
// `map` is separate from snapshot because it only changes on game start
pub fn render(terminal: &mut Term, map: &Map, snapshot: &GameSnapshot) -> io::Result<()> {
    terminal.draw(|frame| {
        let area = frame.area();

        // Split the screen into: main area (top) and help bar (bottom, 3 rows)
        let vertical = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Min(0), Constraint::Length(3)])
            .split(area);

        // Split the main area into: map (left) and sidebar (right, 20 cols)
        let horizontal = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Min(0), Constraint::Length(22)])
            .split(vertical[0]);

        frame.render_widget(build_map_widget(map, snapshot), horizontal[0]);
        frame.render_widget(build_sidebar_widget(snapshot), horizontal[1]);
        frame.render_widget(build_help_widget(), vertical[1]);
    })?;

    Ok(())
}