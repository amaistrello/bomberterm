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
    
        for y in 0..h {
            for x in 0..w {
                if x == 0 || y == 0 || x == w - 1 || y == h - 1 {
                    tiles[y][x] = Tile::Wall;
                } else if x % 2 == 0 && y % 2 == 0 {
                    tiles[y][x] = Tile::Wall;
                }
            }
        }
    
        // Protect 2-tile corridors around all 8 spawn positions
        let spawns: Vec<(usize, usize)> = vec![
            (1, 1), (2, 1), (1, 2),
            (w-2, h-2), (w-3, h-2), (w-2, h-3),
            (1, h-2), (2, h-2), (1, h-3),
            (w-2, 1), (w-3, 1), (w-2, 2),
            // middle spawns
            (1, h/2), (2, h/2),
            (w-2, h/2), (w-3, h/2),
            (w/2, 1), (w/2, 2),
            (w/2, h-2), (w/2, h-3),
        ];
    
        for y in 0..h {
            for x in 0..w {
                if tiles[y][x] == Tile::Empty && !spawns.contains(&(x, y)) {
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