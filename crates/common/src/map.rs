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

        for y in 0..height as usize {
            for x in 0..width as usize {
                if x == 0 || y == 0 || x == width as usize - 1 || y == height as usize - 1 {
                    // Border is always a hard wall
                    tiles[y][x] = Tile::Wall;
                } else if x % 2 == 0 && y % 2 == 0 {
                    // Interior pillars on even coords — classic Bomberman pattern
                    tiles[y][x] = Tile::Wall;
                }
            }
        }

        // Seed destructible blocks, but leave spawn corners clear
        // Spawn positions are the 4 corners (just inside the border)
        let spawns: &[(usize, usize)] = &[(1, 1), (1, 2), (2, 1),
            (width as usize - 2, 1), (width as usize - 3, 1), (width as usize - 2, 2),
            (1, height as usize - 2), (1, height as usize - 3), (2, height as usize - 2),
            (width as usize - 2, height as usize - 2),
            (width as usize - 3, height as usize - 2),
            (width as usize - 2, height as usize - 3),
        ];

        for y in 0..height as usize {
            for x in 0..width as usize {
                if tiles[y][x] == Tile::Empty && !spawns.contains(&(x, y)) {
                    // ~40% chance of a destructible block on empty tiles
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
}