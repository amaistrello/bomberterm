pub mod types;
pub mod map;
pub mod protocol;

#[cfg(test)]
mod tests {
    // `super::*` imports everything from the parent module (this crate)
    use super::protocol::{GameSnapshot, ServerMsg};
    use super::types::{Player, Bomb, Explosion};
    use super::map::Map;

    #[test]
    fn snapshot_roundtrip() {
        // Build a realistic snapshot — same structs the server will produce
        let snapshot = GameSnapshot {
            tick: 42,
            players: vec![
                Player::new(0, "Alice".to_string(), (1, 1)),
                Player::new(1, "Bob".to_string(), (13, 11)),
            ],
            bombs: vec![
                Bomb { owner: 0, pos: (3, 3), timer: 5, range: 2 }
            ],
            explosions: vec![
                Explosion { cells: vec![(3, 3), (4, 3), (2, 3)], ttl: 3 }
            ],
            map: super::map::Map::generate(15, 13),
            phase: super::protocol::GamePhase::Running,
            ready_players: vec![0, 1],
            powerups: vec![],
            death_order: vec![],
        };

        // Serialize to bytes
        // bincode::serialize returns Result<Vec<u8>, _>
        // .expect() will panic with the message if it's an Err — fine in tests
        let bytes = bincode::serialize(&snapshot).expect("serialization failed");

        // Bytes should be non-empty
        assert!(!bytes.is_empty());

        // Print so we can see how small it is — run with: cargo test -- --nocapture
        println!("GameSnapshot serialized to {} bytes", bytes.len());

        // Deserialize back — the type annotation tells bincode what to produce
        let decoded: GameSnapshot = bincode::deserialize(&bytes).expect("deserialization failed");

        // Verify the data survived intact
        assert_eq!(decoded.tick, 42);
        assert_eq!(decoded.players.len(), 2);
        assert_eq!(decoded.players[0].name, "Alice");
        assert_eq!(decoded.players[1].pos, (13, 11));
        assert_eq!(decoded.bombs[0].timer, 5);
        assert_eq!(decoded.explosions[0].cells.len(), 3);

        println!("Round-trip successful!");
    }

    #[test]
    fn server_msg_roundtrip() {
        // Also test that ServerMsg (the wrapper enum) serializes correctly
        // This is what actually travels over the wire
        let map = Map::generate(15, 13);
        let msg = ServerMsg::Welcome {
            your_id: 0,
            you_are_host: true,
            map,
        };

        let bytes = bincode::serialize(&msg).expect("serialization failed");
        println!("ServerMsg::Welcome serialized to {} bytes", bytes.len());

        // Deserialize and pattern match to verify
        let decoded: ServerMsg = bincode::deserialize(&bytes).expect("deserialization failed");

        // `if let` destructures the enum variant — if it's not Welcome, the test fails
        if let ServerMsg::Welcome { your_id, you_are_host, map } = decoded {
            assert_eq!(your_id, 0);
            assert!(you_are_host);
            assert_eq!(map.width, 15);
            assert_eq!(map.height, 13);
            println!("Map tiles: {}x{} grid deserialized correctly", map.width, map.height);
        } else {
            panic!("Expected ServerMsg::Welcome, got something else");
        }
    }

    #[test]
    fn map_generate_sanity() {
        // Test both a small and a large map
        for (w, h) in [(15u16, 13u16), (25, 21), (105, 45)] {
            let map = Map::generate(w, h);
    
            // Borders are walls
            for x in 0..w {
                assert_eq!(map.get(x, 0),     Some(&super::map::Tile::Wall));
                assert_eq!(map.get(x, h - 1), Some(&super::map::Tile::Wall));
            }
            for y in 0..h {
                assert_eq!(map.get(0, y),     Some(&super::map::Tile::Wall));
                assert_eq!(map.get(w - 1, y), Some(&super::map::Tile::Wall));
            }
    
            // All 8 spawn positions must be walkable
            let spawns = [
                (1,       1      ),
                (w - 2,   h - 2  ),
                (1,       h - 2  ),
                (w - 2,   1      ),
                (1,       h / 2  ),
                (w - 2,   h / 2  ),
                (w / 2,   1      ),
                (w / 2,   h - 2  ),
            ];
            for (sx, sy) in spawns {
                assert!(
                    map.is_walkable(sx, sy),
                    "spawn ({}, {}) is not walkable on {}x{} map", sx, sy, w, h
                );
            }
    
            // Interior even pillars are walls
            assert_eq!(map.get(2, 2), Some(&super::map::Tile::Wall));
        }
    }
}