use std::time::Duration;
use crossterm::event::{self, Event, KeyCode};
use common::map::Map;
use common::protocol::GameSnapshot;
use common::types::{Player, Bomb, Explosion};

mod tui;

#[tokio::main]
async fn main() {
    // Build a hardcoded snapshot so we can test rendering
    // without a server connection
    let map = Map::generate(15, 13);

    let snapshot = GameSnapshot {
        tick: 42,
        players: vec![
            Player::new(0, "Alice".to_string(), (1, 1)),
            Player::new(1, "Bob".to_string(),   (13, 11)),
        ],
        bombs: vec![
            Bomb { owner: 0, pos: (3, 3), timer: 3, range: 2 },
        ],
        explosions: vec![
            Explosion { cells: vec![(6, 5), (7, 5), (8, 5), (7, 4), (7, 6)], ttl: 2 },
        ],
    };

    // Set up the terminal — if this fails we want to crash loudly
    let mut terminal = tui::setup().expect("failed to setup terminal");

    // Main render loop
    loop {
        // Draw the current state
        tui::render(&mut terminal, &map, &snapshot)
            .expect("failed to render");

        // Poll for keypresses without blocking
        // Duration::ZERO means: check right now, don't wait
        if event::poll(Duration::from_millis(50)).unwrap_or(false) {
            if let Ok(Event::Key(key)) = event::read() {
                match key.code {
                    KeyCode::Char('q') | KeyCode::Char('Q') => break,
                    _ => {}
                }
            }
        }
    }

    // Always restore the terminal before exiting
    tui::teardown(&mut terminal).expect("failed to teardown terminal");

    println!("Bye!");
}