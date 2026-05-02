use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{broadcast, mpsc};
use tokio::time::{interval, Duration};
use tokio_util::codec::LengthDelimitedCodec;
use tokio_serde::formats::Bincode;
use futures::{SinkExt, StreamExt};
use common::protocol::{ClientMsg, ServerMsg, GameSnapshot};
use common::map::Map;
use common::types::{Player, PlayerId, Bomb, Explosion};
use tracing::{info, warn, error};

type FramedStream = tokio_serde::Framed<tokio_util::codec::Framed<TcpStream, LengthDelimitedCodec>, ClientMsg, ServerMsg, Bincode<ClientMsg, ServerMsg>>;

// All mutable game state lives here, behind Arc<Mutex<>>
struct SharedState {
    next_player_id: PlayerId,
    map: Map,
    players: HashMap<PlayerId, Player>,
    bombs: Vec<Bomb>,
    explosions: Vec<Explosion>,
    tick: u64,
}

impl SharedState {
    fn new() -> Self {
        Self {
            next_player_id: 0,
            map: Map::generate(15, 13),
            players: HashMap::new(),
            bombs: Vec::new(),
            explosions: Vec::new(),
            tick: 0,
        }
    }

    fn add_player(&mut self, name: String) -> PlayerId {
        let id = self.next_player_id;
        self.next_player_id += 1;
        // Each player spawns at a different corner
        let spawn = match id {
            0 => (1,  1),
            1 => (13, 11),
            2 => (1,  11),
            3 => (13, 1),
            _ => (1,  1),
        };
        self.players.insert(id, Player::new(id, name, spawn));
        id
    }

    fn snapshot(&self) -> GameSnapshot {
        GameSnapshot {
            tick: self.tick,
            players: self.players.values().cloned().collect(),
            bombs: self.bombs.clone(),
            explosions: self.explosions.clone(),
            map: self.map.clone(),  // ← add this
        }
    }
}

// An input from a specific player, forwarded from their connection task
struct PlayerInput {
    player_id: PlayerId,
    direction: Option<common::types::Direction>,
    place_bomb: bool,
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt::init();

    let addr = "127.0.0.1:7777";
    let listener = TcpListener::bind(addr).await.expect("failed to bind");
    info!("Server listening on {}", addr);

    let state = Arc::new(Mutex::new(SharedState::new()));

    // broadcast: game loop → all connections → all clients
    // capacity 16: a slow client can lag 16 ticks before getting a Lagged error
    let (snapshot_tx, _) = broadcast::channel::<GameSnapshot>(16);

    // mpsc: all connections → game loop (inputs)
    let (input_tx, input_rx) = mpsc::channel::<PlayerInput>(64);

    tokio::spawn(game_loop(state.clone(), snapshot_tx.clone(), input_rx));

    loop {
        match listener.accept().await {
            Ok((stream, addr)) => {
                info!("New connection from {}", addr);
                tokio::spawn(handle_connection(
                    stream,
                    addr,
                    state.clone(),
                    snapshot_tx.subscribe(), // each connection gets its own receiver
                    input_tx.clone(),
                ));
            }
            Err(e) => error!("Accept error: {}", e),
        }
    }
}

async fn game_loop(
    state: Arc<Mutex<SharedState>>,
    snapshot_tx: broadcast::Sender<GameSnapshot>,
    mut input_rx: mpsc::Receiver<PlayerInput>,
) {
    let mut ticker = interval(Duration::from_millis(100));

    loop {
        ticker.tick().await;

        let snapshot = {
            let mut s = state.lock().unwrap();
            s.tick += 1;

            // ── 1. Process inputs ──────────────────────────────────────────

            while let Ok(input) = input_rx.try_recv() {
                // Compute new position before mutably borrowing players
                let new_pos = if let Some(player) = s.players.get(&input.player_id) {
                    if player.alive {
                        input.direction.and_then(|dir| {
                            let (dx, dy) = dir.delta();
                            let nx = player.pos.0 as i16 + dx;
                            let ny = player.pos.1 as i16 + dy;
                            if nx >= 0 && ny >= 0 { Some((nx as u16, ny as u16)) }
                            else { None }
                        })
                    } else { None }
                } else { None };

                // Apply movement if the destination is walkable and not occupied by a bomb
                if let Some((nx, ny)) = new_pos {
                    let bomb_there = s.bombs.iter().any(|b| b.pos == (nx, ny));
                    if s.map.is_walkable(nx, ny) && !bomb_there {
                        if let Some(player) = s.players.get_mut(&input.player_id) {
                            player.pos = (nx, ny);
                        }
                    }
                }

                // Place a bomb if requested
                if input.place_bomb {
                    let can_place = s.players.get(&input.player_id).map_or(false, |p| {
                        p.alive && p.bombs_placed < p.max_bombs
                    });
                    if can_place {
                        let (pos, range) = {
                            let p = s.players.get(&input.player_id).unwrap();
                            (p.pos, p.bomb_range)
                        };
                        // Don't allow placing two bombs on the same tile
                        let already = s.bombs.iter().any(|b| b.pos == pos);
                        if !already {
                            s.bombs.push(Bomb {
                                owner: input.player_id,
                                pos,
                                timer: 30, // 30 ticks × 100ms = 3 seconds
                                range,
                            });
                            if let Some(p) = s.players.get_mut(&input.player_id) {
                                p.bombs_placed += 1;
                            }
                        }
                    }
                }
            }

            // ── 2. Tick bombs, collect detonations ─────────────────────────

            // Separate which bombs explode from which survive
            // We do this in two passes to avoid mutating while iterating
            let mut surviving = Vec::new();
            let mut detonating = Vec::new();

            for mut bomb in s.bombs.drain(..) {
                bomb.timer = bomb.timer.saturating_sub(1);
                if bomb.timer == 0 {
                    detonating.push(bomb);
                } else {
                    surviving.push(bomb);
                }
            }
            s.bombs = surviving;

            // ── 3. Calculate explosion cells ───────────────────────────────

            // We process detonations in a queue so chain reactions work:
            // if an explosion hits another bomb, that bomb detonates too
            let mut detonate_queue = detonating;

            while let Some(bomb) = detonate_queue.pop() {
                // Return the bomb's slot to the owner
                if let Some(p) = s.players.get_mut(&bomb.owner) {
                    p.bombs_placed = p.bombs_placed.saturating_sub(1);
                }

                let mut cells: Vec<(u16, u16)> = vec![bomb.pos];

                // Cast rays in all 4 directions
                let directions: &[(i16, i16)] = &[(0,-1),(0,1),(-1,0),(1,0)];

                for &(dx, dy) in directions {
                    for step in 1..=bomb.range as i16 {
                        let nx = bomb.pos.0 as i16 + dx * step;
                        let ny = bomb.pos.1 as i16 + dy * step;

                        if nx < 0 || ny < 0 { break; }
                        let (nx, ny) = (nx as u16, ny as u16);

                        match s.map.get(nx, ny) {
                            Some(common::map::Tile::Wall) => {
                                // Hard wall stops the ray, cell not included
                                break;
                            }
                            Some(common::map::Tile::Destructible) => {
                                // Destroys the block but doesn't penetrate further
                                cells.push((nx, ny));
                                s.map.destroy(nx, ny);
                                break;
                            }
                            Some(common::map::Tile::Empty) => {
                                cells.push((nx, ny));

                                // Chain reaction: if there's a bomb here, detonate it
                                if let Some(idx) = s.bombs.iter().position(|b| b.pos == (nx, ny)) {
                                    let chained = s.bombs.remove(idx);
                                    detonate_queue.push(chained);
                                }
                            }
                            None => break,
                        }
                    }
                }

                s.explosions.push(Explosion { cells, ttl: 5 }); // 5 ticks = 0.5 seconds
            }

            // ── 4. Kill players caught in explosions ───────────────────────

            let explosion_cells: Vec<(u16, u16)> = s.explosions
                .iter()
                .flat_map(|e| e.cells.iter().copied())
                .collect();

            for player in s.players.values_mut() {
                if player.alive && explosion_cells.contains(&player.pos) {
                    player.alive = false;
                    info!("Player {} {} was killed", player.id, player.name);
                }
            }

            // ── 5. Tick explosion TTL ──────────────────────────────────────

            for exp in s.explosions.iter_mut() {
                exp.ttl = exp.ttl.saturating_sub(1);
            }
            s.explosions.retain(|e| e.ttl > 0);

            s.snapshot()
        };

        let _ = snapshot_tx.send(snapshot);
    }
}

async fn handle_connection(
    stream: TcpStream,
    addr: SocketAddr,
    state: Arc<Mutex<SharedState>>,
    mut snapshot_rx: broadcast::Receiver<GameSnapshot>,
    input_tx: mpsc::Sender<PlayerInput>,
) {
    let mut framed = make_framed(stream);

    // First message must be Join
    let player_id = match framed.next().await {
        Some(Ok(ClientMsg::Join { name })) => {
            info!("{} joined from {}", name, addr);
        
            // Guard lives only inside this block — provably dropped before the await below
            let (id, map) = {
                let mut s = state.lock().unwrap();
                let id = s.add_player(name.clone());
                let map = s.map.clone();
                (id, map)
            };
        
            let welcome = ServerMsg::Welcome {
                your_id: id,
                you_are_host: id == 0,
                map,
            };
            if let Err(e) = framed.send(welcome).await {
                error!("Failed to send Welcome to {}: {}", name, e);
                return;
            }
            info!("Welcomed {} as player {}", name, id);
            id
        }
        _ => {
            warn!("Expected Join from {} — dropping", addr);
            return;
        }
    };

    // select! waits for EITHER branch to be ready, handles it, then loops.
    // This lets one task handle both directions of the connection concurrently.
    loop {
        tokio::select! {
            // Game loop produced a snapshot — send it to this client
            result = snapshot_rx.recv() => {
                match result {
                    Ok(snapshot) => {
                        if let Err(e) = framed.send(ServerMsg::StateUpdate(snapshot)).await {
                            error!("Send to {} failed: {}", addr, e);
                            break;
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(n)) => {
                        warn!("{} lagged {} ticks", addr, n);
                        // Not fatal — just missed some frames, keep going
                    }
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }

            // Client sent an input — forward it to the game loop
            msg = framed.next() => {
                match msg {
                    Some(Ok(ClientMsg::Input { direction, place_bomb })) => {
                        let _ = input_tx.send(PlayerInput { player_id, direction, place_bomb }).await;
                    }
                    Some(Err(e)) => { error!("Decode error from {}: {}", addr, e); break; }
                    None => { info!("{} disconnected", addr); break; }
                    _ => {}
                }
            }
        }
    }

    // Remove the player from state when they disconnect
    state.lock().unwrap().players.remove(&player_id);
    info!("Player {} removed", player_id);
}

fn make_framed(stream: TcpStream) -> FramedStream {
    let ld = tokio_util::codec::Framed::new(stream, LengthDelimitedCodec::new());
    tokio_serde::Framed::new(ld, Bincode::<ClientMsg, ServerMsg>::default())
}