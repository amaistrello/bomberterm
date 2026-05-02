use serde::{Deserialize, Serialize};

// A simple type alias — instead of writing (u16, u16) everywhere
// we write Position. Makes intent clear.
pub type Position = (u16, u16);

// Another alias — a player's unique ID is just a u8 (max 255 players, fine)
pub type PlayerId = u8;

// #[derive(...)] auto-generates boilerplate for these traits:
// - Serialize/Deserialize: so we can send this over the network
// - Clone/Copy: so we can duplicate values cheaply
// - Debug: so we can print it with {:?}
// - PartialEq: so we can compare with ==
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq)]
pub enum Direction {
    Up,
    Down,
    Left,
    Right,
}

impl Direction {
    // Returns how a direction changes (x, y) position
    // i16 because movement can be negative (going Up = y-1)
    pub fn delta(&self) -> (i16, i16) {
        match self {
            Direction::Up    => (0, -1),
            Direction::Down  => (0,  1),
            Direction::Left  => (-1, 0),
            Direction::Right => (1,  0),
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Player {
    pub id: PlayerId,
    pub name: String,
    pub pos: Position,
    pub alive: bool,
    pub bomb_range: u8,   // how far explosions reach
    pub max_bombs: u8,    // how many bombs placeable at once
    pub bombs_placed: u8, // how many currently on the map
    pub speed: u8,
    pub last_moved_tick: u64,
}

impl Player {
    pub fn new(id: PlayerId, name: String, pos: Position) -> Self {
        Self {
            id,
            name,
            pos,
            alive: true,
            bomb_range: 2,
            max_bombs: 1,
            bombs_placed: 0,
            speed: 1,
            last_moved_tick: 0,
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Bomb {
    pub owner: PlayerId,
    pub pos: Position,
    pub timer: u8,  // ticks until detonation
    pub range: u8,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Explosion {
    pub cells: Vec<Position>, // every tile the explosion covers
    pub ttl: u8,              // ticks until it disappears
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub enum PowerupKind {
    ExtraBomb,
    LongerRange,
    Speed,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Powerup {
    pub pos: Position,
    pub kind: PowerupKind,
}