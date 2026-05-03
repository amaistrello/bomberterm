use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub enum Tile {
    Empty,
    Wall,         // indestructible
    Destructible, // blown up by explosions
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Map {
    pub width: u16,
    pub height: u16,
    // Vec<Vec<T>> is a 2D grid: tiles[y][x]
    pub tiles: Vec<Vec<Tile>>,
}

impl Map {
    // Generates a classic Bomberman layout:
    // hard walls on every even (x, y), destructible blocks randomly placed
    pub fn generate(width: u16, height: u16) -> Self {
        let mut tiles = vec![vec![Tile::Empty; width as usize]; height as usize];
        let w = width as usize;
        let h = height as usize;
    
        // Step 1 — hard walls
        for y in 0..h {
            for x in 0..w {
                if x == 0 || y == 0 || x == w - 1 || y == h - 1 {
                    tiles[y][x] = Tile::Wall;
                } else if x % 2 == 0 && y % 2 == 0 {
                    tiles[y][x] = Tile::Wall;
                }
            }
        }
    
        // Step 2 — build protected zones around all 8 spawn positions
        // These mirror spawn_pos() exactly so they always match
        let spawns_logical: Vec<(usize, usize)> = vec![
            (1,         1        ),   // player 0 — top left
            (w - 2,     h - 2    ),   // player 1 — bottom right
            (1,         h - 2    ),   // player 2 — bottom left
            (w - 2,     1        ),   // player 3 — top right
            (1,         h / 2    ),   // player 4 — middle left
            (w - 2,     h / 2    ),   // player 5 — middle right
            (w / 2,     1        ),   // player 6 — top middle
            (w / 2,     h - 2    ),   // player 7 — bottom middle
        ];
    
        // For each spawn, protect a 2-tile L-shaped corridor so the player
        // can always move in at least two directions immediately
        let mut protected: Vec<(usize, usize)> = Vec::new();
        for (sx, sy) in &spawns_logical {
            let sx = *sx;
            let sy = *sy;
    
            // The spawn tile itself
            protected.push((sx, sy));
    
            // Two tiles away from each border wall in both axes
            // This guarantees the player has room to move without
            // immediately walking into a destructible block
            let left  = sx.saturating_sub(2);
            let right = (sx + 2).min(w - 1);
            let up    = sy.saturating_sub(2);
            let down  = (sy + 2).min(h - 1);
    
            // Horizontal corridor
            for x in left..=right {
                protected.push((x, sy));
            }
    
            // Vertical corridor
            for y in up..=down {
                protected.push((sx, y));
            }
        }
    
        // Step 3 — fill non-protected empty tiles with destructible blocks
        // Use a simple deterministic pattern based on position
        // so the map looks the same every time for the same dimensions
        for y in 1..h - 1 {
            for x in 1..w - 1 {
                if tiles[y][x] == Tile::Empty && !protected.contains(&(x, y)) {
                    // ~40% chance — feels dense without being suffocating
                    if (x * 7 + y * 13) % 10 < 4 {
                        tiles[y][x] = Tile::Destructible;
                    }
                }
            }
        }
    
        Self { width, height, tiles }
    }

    pub fn get(&self, x: u16, y: u16) -> Option<&Tile> {
        self.tiles.get(y as usize)?.get(x as usize)
    }

    pub fn set(&mut self, x: u16, y: u16, tile: Tile) {
        if let Some(row) = self.tiles.get_mut(y as usize) {
            if let Some(cell) = row.get_mut(x as usize) {
                *cell = tile;
            }
        }
    }

    pub fn is_walkable(&self, x: u16, y: u16) -> bool {
        matches!(self.get(x, y), Some(Tile::Empty))
    }

    pub fn destroy(&mut self, x: u16, y: u16) {
        // Only destructible tiles can be destroyed
        if let Some(Tile::Destructible) = self.get(x, y) {
            self.set(x, y, Tile::Empty);
        }
    }
}