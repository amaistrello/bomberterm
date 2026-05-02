use serde::{Deserialize, Serialize};
use crate::types::{Direction, Player, Bomb, Explosion, PlayerId};
use crate::map::Map;

// Messages the CLIENT sends to the server
#[derive(Serialize, Deserialize, Clone, Debug)]
pub enum ClientMsg {
    // Sent once, right after connecting
    Join { name: String },
    // Sent every time the player does something
    // Option<Direction> because the player might place a bomb without moving
    Input {
        direction: Option<Direction>,
        place_bomb: bool,
    },
}

// Messages the SERVER sends to clients
#[derive(Serialize, Deserialize, Clone, Debug)]
pub enum ServerMsg {
    // Sent to a new player when they successfully join
    Welcome {
        your_id: PlayerId,
        you_are_host: bool,
        map: Map,
    },
    // Sent every tick to all clients — the full world state
    StateUpdate(GameSnapshot),
    // Lobby updates while waiting for players
    LobbyUpdate { players: Vec<String> },
    // Game over
    GameOver { winner: Option<PlayerId> },
    // Server rejected the connection (e.g. game already started)
    Rejected { reason: String },
}

// A complete snapshot of the game at one tick
// This is what gets sent to clients every tick
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct GameSnapshot {
    pub tick: u64,
    pub players: Vec<Player>,
    pub bombs: Vec<Bomb>,
    pub explosions: Vec<Explosion>,
}