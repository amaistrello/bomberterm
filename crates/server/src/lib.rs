use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{broadcast, mpsc};
use tokio::time::{interval, Duration};
use tokio_util::codec::LengthDelimitedCodec;
use tokio_serde::formats::Bincode;
use futures::{SinkExt, StreamExt};
use common::protocol::{ClientMsg, ServerMsg, GameSnapshot, GamePhase};
use common::map::Map;
use common::types::{Player, PlayerId, Bomb, Explosion, Direction, Powerup, PowerupKind};
use tracing::{info, warn, error};
use std::net::UdpSocket;
use common::protocol::Beacon;

pub const DISCOVERY_PORT: u16 = 7778;

type FramedStream = tokio_serde::Framed<tokio_util::codec::Framed<TcpStream, LengthDelimitedCodec>, ClientMsg, ServerMsg, Bincode<ClientMsg, ServerMsg>>;

pub struct ServerConfig {
    pub port: u16,
    pub max_players: u8,
    pub map_width: u16,
    pub map_height: u16,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            port: 7777,
            max_players: 8,
            map_width: 25,
            map_height: 21,
        }
    }
}

struct SharedState {
    next_player_id: PlayerId,
    map: Map,
    map_width: u16,
    map_height: u16,
    players: HashMap<PlayerId, Player>,
    bombs: Vec<Bomb>,
    explosions: Vec<Explosion>,
    powerups: Vec<Powerup>,
    tick: u64,
    phase: GamePhase,
    ready_players: Vec<PlayerId>,
}

impl SharedState {
    fn new(map_width: u16, map_height: u16) -> Self {
        Self {
            next_player_id: 0,
            map: Map::generate(map_width, map_height),
            map_width,
            map_height,
            players: HashMap::new(),
            bombs: Vec::new(),
            explosions: Vec::new(),
            powerups: Vec::new(),
            tick: 0,
            phase: GamePhase::Lobby,
            ready_players: Vec::new(),
        }
    }

    fn add_player(&mut self, name: String) -> PlayerId {
        let id = self.next_player_id;
        self.next_player_id += 1;
        let spawn = spawn_pos(id);
        self.players.insert(id, Player::new(id, name, spawn));
        id
    }

    fn snapshot(&self) -> GameSnapshot {
        GameSnapshot {
            tick: self.tick,
            players: self.players.values().cloned().collect(),
            bombs: self.bombs.clone(),
            explosions: self.explosions.clone(),
            map: self.map.clone(),
            phase: self.phase.clone(),
            ready_players: self.ready_players.clone(),
            powerups: self.powerups.clone()
        }
    }

    fn check_win_condition(&mut self) {
        if self.phase != GamePhase::Running { return; }
        let alive: Vec<PlayerId> = self.players.values()
            .filter(|p| p.alive).map(|p| p.id).collect();
        match alive.len() {
            1 => { self.phase = GamePhase::GameOver { winner: Some(alive[0]) }; }
            0 => { self.phase = GamePhase::GameOver { winner: None }; }
            _ => {}
        }
    }

    fn try_start(&mut self) {
        if self.phase != GamePhase::Lobby { return; }
        if self.players.len() < 2 { return; }

        let all_ready = self.players.keys()
            .all(|id| self.ready_players.contains(id));

        if all_ready {
            self.phase = GamePhase::Running;
            info!("All {} players ready — game starting!", self.players.len());
        }
    }
}


fn spawn_pos(id: PlayerId) -> (u16, u16) {
    match id % 8 {
        0 => (1,  1),
        1 => (23, 19),
        2 => (1,  19),
        3 => (23, 1),
        4 => (1,  10),  // middle left
        5 => (23, 10),  // middle right
        6 => (12, 1),   // top middle
        _ => (12, 19),  // bottom middle
    }
}

struct PlayerInput {
    player_id: PlayerId,
    direction: Option<Direction>,
    place_bomb: bool,
}

// Public entry point — called by the client when hosting
pub async fn run(config: ServerConfig) {
    let addr = format!("0.0.0.0:{}", config.port);
    let listener = TcpListener::bind(&addr).await.expect("failed to bind");
    info!("Server listening on {}", addr);

    let state = Arc::new(Mutex::new(SharedState::new(config.map_width, config.map_height)));
    let (snapshot_tx, _) = broadcast::channel::<GameSnapshot>(16);
    let (input_tx, input_rx) = mpsc::channel::<PlayerInput>(64);

    tokio::spawn(game_loop(state.clone(), snapshot_tx.clone(), input_rx));
    tokio::spawn(broadcast_beacon(config.port, state.clone()));

    loop {
        match listener.accept().await {
            Ok((stream, addr)) => {
                info!("New connection from {}", addr);
                tokio::spawn(handle_connection(
                    stream, addr,
                    state.clone(),
                    snapshot_tx.subscribe(),
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

            // 1. Process inputs
            while let Ok(input) = input_rx.try_recv() {
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
                        let current_tick = s.tick;
                        if let Some(player) = s.players.get_mut(&input.player_id) {
                            // How many ticks must pass between moves:
                            // speed 1 = every 2 ticks, speed 2 = every tick, speed 3 = every tick (no extra cap)
                            let move_interval = match player.speed {
                                1 => 2,
                                _ => 1,
                            };
                            if current_tick.saturating_sub(player.last_moved_tick) >= move_interval {
                                player.pos = (nx, ny);
                                player.last_moved_tick = current_tick;
                            }
                        }
                    }
                }

                if input.place_bomb {
                    let can_place = s.players.get(&input.player_id)
                        .map_or(false, |p| p.alive && p.bombs_placed < p.max_bombs);
                    if can_place {
                        let (pos, range) = {
                            let p = s.players.get(&input.player_id).unwrap();
                            (p.pos, p.bomb_range)
                        };
                        if !s.bombs.iter().any(|b| b.pos == pos) {
                            s.bombs.push(Bomb { owner: input.player_id, pos, timer: 30, range });
                            if let Some(p) = s.players.get_mut(&input.player_id) {
                                p.bombs_placed += 1;
                            }
                        }
                    }
                }
            }

            // 2. Tick bombs
            let mut surviving = Vec::new();
            let mut detonating = Vec::new();
            for mut bomb in s.bombs.drain(..) {
                bomb.timer = bomb.timer.saturating_sub(1);
                if bomb.timer == 0 { detonating.push(bomb); }
                else { surviving.push(bomb); }
            }
            s.bombs = surviving;

            // ── 3. Explode ─────────────────────────────────────────────
            let mut new_powerups: Vec<Powerup> = Vec::new();
            let mut newly_exploded_cells: Vec<(u16, u16)> = Vec::new();
            
            let mut detonate_queue = detonating;
            while let Some(bomb) = detonate_queue.pop() {
                if let Some(p) = s.players.get_mut(&bomb.owner) {
                    p.bombs_placed = p.bombs_placed.saturating_sub(1);
                }
                let mut cells = vec![bomb.pos];
                for &(dx, dy) in &[(0i16,-1),(0,1),(-1,0),(1,0)] {
                    for step in 1..=bomb.range as i16 {
                        let nx = bomb.pos.0 as i16 + dx * step;
                        let ny = bomb.pos.1 as i16 + dy * step;
                        if nx < 0 || ny < 0 { break; }
                        let (nx, ny) = (nx as u16, ny as u16);
                        match s.map.get(nx, ny) {
                            Some(common::map::Tile::Wall) => break,
                            Some(common::map::Tile::Destructible) => {
                                cells.push((nx, ny));
                                s.map.destroy(nx, ny);
                                let roll = (nx as u64 * 31 + ny as u64 * 17 + s.tick * 7) % 100;
                                if roll < 35 {
                                    let kind = match roll % 3 {
                                        0 => PowerupKind::ExtraBomb,
                                        1 => PowerupKind::LongerRange,
                                        _ => PowerupKind::Speed,
                                    };
                                    if !new_powerups.iter().any(|p| p.pos == (nx, ny)) {
                                        new_powerups.push(Powerup { pos: (nx, ny), kind });
                                    }
                                }
                                break;
                            }
                            Some(common::map::Tile::Empty) => {
                                cells.push((nx, ny));
                                if let Some(idx) = s.bombs.iter().position(|b| b.pos == (nx, ny)) {
                                    let chained = s.bombs.remove(idx);
                                    detonate_queue.push(chained);
                                }
                            }
                            None => break,
                        }
                    }
                }
                // Track which cells exploded THIS tick specifically
                newly_exploded_cells.extend(cells.iter().copied());
                s.explosions.push(Explosion { cells, ttl: 5 });
            }
            
            // Only remove powerups hit by THIS tick's explosions — not lingering ones
            s.powerups.retain(|p| !newly_exploded_cells.contains(&p.pos));
            
            // Freshly dropped powerups are safe — they were spawned after the retain
            s.powerups.extend(new_powerups);

            // 4. Kill players
            let explosion_cells: Vec<(u16, u16)> = s.explosions
                .iter().flat_map(|e| e.cells.iter().copied()).collect();
            for player in s.players.values_mut() {
                if player.alive && explosion_cells.contains(&player.pos) {
                    player.alive = false;
                    info!("Player {} killed", player.id);
                }
            }

            // 4.5 Powerup pickups
            let mut picked_up = Vec::new();

            // Destructuring 's' allows simultaneous mutable access to its fields.
            let SharedState { players, powerups, .. } = &mut *s;

            for player in players.values_mut() {
                if !player.alive {
                    continue;
                }

                // Now 'powerups' and 'player' (from 'players') are seen as disjoint borrows
                if let Some(idx) = powerups.iter().position(|p| p.pos == player.pos) {
                    let powerup = powerups.remove(idx);

                    match powerup.kind {
                        PowerupKind::ExtraBomb => {
                            player.max_bombs = (player.max_bombs + 1).min(8);
                            info!("Player {} picked up ExtraBomb (max={})", player.id, player.max_bombs);
                        }
                        PowerupKind::LongerRange => {
                            player.bomb_range = (player.bomb_range + 1).min(10);
                            info!("Player {} picked up LongerRange (range={})", player.id, player.bomb_range);
                        }
                        PowerupKind::Speed => {
                            player.speed = (player.speed + 1).min(3);
                            info!("Player {} picked up Speed (speed={})", player.id, player.speed);
                        }
                    }
                    picked_up.push(player.id);
                }
            }

            // 5. Tick explosions
            for exp in s.explosions.iter_mut() {
                exp.ttl = exp.ttl.saturating_sub(1);
            }
            s.explosions.retain(|e| e.ttl > 0);

            // 6. Win condition
            s.check_win_condition();

            // 7. Rematch
            if let GamePhase::GameOver { .. } = s.phase {
                if s.tick % 50 == 0 && s.tick > 0 {
                    info!("Resetting for rematch");
                    s.map = Map::generate(s.map_width, s.map_height);
                    s.bombs.clear();
                    s.powerups.clear();
                    s.explosions.clear();
                    s.tick = 0;
                    s.ready_players.clear();
                    let ids: Vec<PlayerId> = s.players.keys().copied().collect();
                    for (i, id) in ids.iter().enumerate() {
                        if let Some(p) = s.players.get_mut(id) {
                            p.alive = true;
                            p.pos = spawn_pos(i as PlayerId);
                            p.bombs_placed = 0;
                            p.last_moved_tick = 0;
                        }
                    }
                    s.phase = GamePhase::Lobby;
                }
            }

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

    let player_id = match framed.next().await {
        Some(Ok(ClientMsg::Join { name })) => {
            info!("{} joined from {}", name, addr);
            let (id, map) = {
                let mut s = state.lock().unwrap();
                let id = s.add_player(name.clone());
                let map = s.map.clone();
                (id, map)
            };
            let welcome = ServerMsg::Welcome { your_id: id, you_are_host: id == 0, map };
            if let Err(e) = framed.send(welcome).await {
                error!("Failed to send Welcome to {}: {}", name, e);
                return;
            }
            info!("Welcomed {} as player {}", name, id);
            id
        }
        _ => { warn!("Expected Join from {} — dropping", addr); return; }
    };

    loop {
        tokio::select! {
            result = snapshot_rx.recv() => {
                match result {
                    Ok(snapshot) => {
                        if let Err(e) = framed.send(ServerMsg::StateUpdate(snapshot)).await {
                            error!("Send error: {}", e);
                            break;
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(n)) => warn!("Lagged {} ticks", n),
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
            msg = framed.next() => {
                match msg {
                    Some(Ok(ClientMsg::Input { direction, place_bomb })) => {
                        let _ = input_tx.send(PlayerInput { player_id, direction, place_bomb }).await;
                    }
                    Some(Ok(ClientMsg::Ready)) => {
                        let mut s = state.lock().unwrap();
                        if !s.ready_players.contains(&player_id) {
                            s.ready_players.push(player_id);
                            info!("Player {} is ready ({}/{})", player_id, s.ready_players.len(), s.players.len());
                        }
                        s.try_start();
                    }
                    Some(Err(e)) => { error!("Decode error: {}", e); break; }
                    None => { info!("{} disconnected", addr); break; }
                    _ => {}
                }
            }
        }
    }

    state.lock().unwrap().players.remove(&player_id);
    info!("Player {} removed", player_id);
}

fn make_framed(stream: TcpStream) -> FramedStream {
    let ld = tokio_util::codec::Framed::new(stream, LengthDelimitedCodec::new());
    tokio_serde::Framed::new(ld, Bincode::<ClientMsg, ServerMsg>::default())
}

async fn broadcast_beacon(tcp_port: u16, state: Arc<Mutex<SharedState>>) {
    // Bind to any port for sending — we just need to blast out UDP broadcasts
    let socket = match UdpSocket::bind("0.0.0.0:0") {
        Ok(s) => s,
        Err(e) => { error!("Failed to bind UDP socket: {}", e); return; }
    };

    if let Err(e) = socket.set_broadcast(true) {
        error!("Failed to enable broadcast: {}", e);
        return;
    }

    let broadcast_addr = format!("255.255.255.255:{}", DISCOVERY_PORT);
    let mut ticker = interval(Duration::from_secs(2));

    loop {
        ticker.tick().await;

        let beacon = {
            let s = state.lock().unwrap();
            Beacon {
                game_name: s.players.values()
                    .find(|p| p.id == 0)
                    .map(|p| format!("{}'s game", p.name))
                    .unwrap_or_else(|| "BomberTerm game".to_string()),
                host_addr: format!("127.0.0.1:{}", tcp_port),
                players_current: s.players.len() as u8,
                players_max: 4,
                phase: s.phase.clone(),
            }
        };

        match bincode::serialize(&beacon) {
            Ok(bytes) => {
                let bytes: Vec<u8> = bytes;
                let bytes_clone = bytes.clone();
                let addr_clone = broadcast_addr.clone();
                let socket_ref = socket.try_clone().unwrap();
                tokio::task::spawn_blocking(move || {
                    let _ = socket_ref.send_to(&bytes_clone, &addr_clone);
                });
            }
            Err(e) => error!("Failed to serialize beacon: {}", e),
        }
    }
}
