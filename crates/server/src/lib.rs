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
    pub host_name: String
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            port: 7777,
            max_players: 8,
            map_width: 25,
            map_height: 21,
            host_name: "Host".to_string()
        }
    }
}

struct SharedState {
    next_player_id: PlayerId,
    map: Map,
    // Current map dimensions — re-sized to the player count at each match start.
    map_width: u16,
    map_height: u16,
    // Upper bound derived from the host's terminal, so the map never overflows
    // the screen no matter how many players join.
    max_width: u16,
    max_height: u16,
    players: HashMap<PlayerId, Player>,
    bombs: Vec<Bomb>,
    explosions: Vec<Explosion>,
    powerups: Vec<Powerup>,
    tick: u64,
    phase: GamePhase,
    ready_players: Vec<PlayerId>,
    death_order: Vec<PlayerId>, 
}

impl SharedState {
    fn new(max_width: u16, max_height: u16) -> Self {
        Self {
            next_player_id: 0,
            // Generated at full size for now; resized to the player count when the match starts.
            map: Map::generate(max_width, max_height),
            map_width: max_width,
            map_height: max_height,
            max_width,
            max_height,
            players: HashMap::new(),
            bombs: Vec::new(),
            explosions: Vec::new(),
            powerups: Vec::new(),
            tick: 0,
            phase: GamePhase::Lobby,
            ready_players: Vec::new(),
            death_order: Vec::new(), 
        }
    }

    fn add_player(&mut self, name: String) -> PlayerId {
        let id = self.next_player_id;
        self.next_player_id += 1;
        let spawn = spawn_pos(id, self.map_width, self.map_height);
        let mut player = Player::new(id, name, spawn);

        player.bomb_range = bomb_range_for_width(self.map_width);

        self.players.insert(id, player);
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
            powerups: self.powerups.clone(),
            death_order: self.death_order.clone()
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
            self.start_match();
        }
    }

    // Begins the match: sizes the map to the number of players (capped to the
    // host's terminal), regenerates it, and places every player at a fresh spawn.
    fn start_match(&mut self) {
        let (w, h) = map_size_for_players(self.players.len(), self.max_width, self.max_height);
        self.map_width = w;
        self.map_height = h;
        self.map = Map::generate(w, h);

        // Sort ids so spawn assignment is deterministic and players are spread out.
        let mut ids: Vec<PlayerId> = self.players.keys().copied().collect();
        ids.sort();
        for (i, id) in ids.iter().enumerate() {
            if let Some(p) = self.players.get_mut(id) {
                p.pos = spawn_pos(i as PlayerId, w, h);
                p.bomb_range = bomb_range_for_width(w);
                p.last_moved_tick = 0;
            }
        }

        self.phase = GamePhase::Running;
        info!("All {} players ready — game starting on {}x{} map!", self.players.len(), w, h);
    }
}


// Bomb range scales gently with map width so big maps don't feel sparse.
fn bomb_range_for_width(w: u16) -> u8 {
    ((w / 15) + 1).min(8) as u8
}

// Picks map dimensions from the number of players in the match. A 2-player game
// is fairly small and tight; each extra player adds ~2 tiles per axis, topping
// out a bit above the classic 25x21 layout for a full 8-player lobby. The result
// is capped to the host's terminal-derived maximum, floored to a playable
// minimum, and forced odd so the hard-wall pillar pattern lines up.
fn map_size_for_players(num_players: usize, max_w: u16, max_h: u16) -> (u16, u16) {
    let n = (num_players.clamp(2, 8)) as u16;

    let mut w = (11 + 2 * n).min(max_w).max(15); // 2p -> 15 ... 8p -> 27
    let mut h = (9 + 2 * n).min(max_h).max(13);  // 2p -> 13 ... 8p -> 25

    if w % 2 == 0 { w -= 1; }
    if h % 2 == 0 { h -= 1; }
    (w, h)
}

fn spawn_pos(id: PlayerId, w: u16, h: u16) -> (u16, u16) {
    match id % 8 {
        0 => (1,       1      ),
        1 => (w - 2,   h - 2  ),
        2 => (1,       h - 2  ),
        3 => (w - 2,   1      ),
        4 => (1,       h / 2  ),
        5 => (w - 2,   h / 2  ),
        6 => (w / 2,   1      ),
        _ => (w / 2,   h - 2  ),
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
    tokio::spawn(broadcast_beacon(config.port, config.host_name.clone(), state.clone()));

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
            
            let mut newly_dead: Vec<PlayerId> = Vec::new();
            for player in s.players.values_mut() {
                if player.alive && explosion_cells.contains(&player.pos) {
                    player.alive = false;
                    newly_dead.push(player.id);
                    info!("Player {} killed", player.id);
                }
            }
            s.death_order.extend(newly_dead);

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
                if s.tick % 100 == 0 && s.tick > 0 {
                    info!("Resetting for rematch");
                    // Back to the lobby — the map is regenerated and resized to the
                    // player count by start_match() once everyone readies up again.
                    s.bombs.clear();
                    s.powerups.clear();
                    s.explosions.clear();
                    s.tick = 0;
                    s.ready_players.clear();
                    s.death_order.clear();
                    for p in s.players.values_mut() {
                        p.alive = true;
                        p.bombs_placed = 0;
                        p.last_moved_tick = 0;
                        p.max_bombs = 1;
                        p.speed = 1;
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

async fn broadcast_beacon(tcp_port: u16, host_name: String, state: Arc<Mutex<SharedState>>) {
    let socket = match UdpSocket::bind("0.0.0.0:0") {
        Ok(s) => s,
        Err(e) => { error!("Failed to bind UDP socket: {}", e); return; }
    };

    if let Err(e) = socket.set_broadcast(true) {
        error!("Failed to enable broadcast: {}", e);
        return;
    }

    let broadcast_addr = format!("255.255.255.255:{}", DISCOVERY_PORT);
    let game_name = format!("{}'s game", host_name);
    let tcp_addr = format!("{}:{}", local_ip(), tcp_port);
    info!("Beacon will advertise TCP address: {}", tcp_addr);

    let mut ticker = interval(Duration::from_secs(2));

    loop {
        ticker.tick().await;

        let beacon = {
            let s = state.lock().unwrap();
            Beacon {
                game_name: game_name.clone(),
                host_addr: tcp_addr.clone(),
                players_current: s.players.len() as u8,
                players_max: 8,
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

fn local_ip() -> String {
    // Trick: open a UDP socket and "connect" to a public address.
    // No data is sent — but the OS picks the right local interface
    // and we can read its IP from the socket.
    let socket = std::net::UdpSocket::bind("0.0.0.0:0").ok();
    socket.and_then(|s| {
        s.connect("8.8.8.8:80").ok()?;
        s.local_addr().ok()
    })
    .map(|addr| addr.ip().to_string())
    .unwrap_or_else(|| "127.0.0.1".to_string())
}
#[cfg(test)]
mod tests {
    use super::{map_size_for_players, spawn_pos};
    use common::map::Map;

    #[test]
    fn map_grows_with_player_count() {
        // With a generous cap, more players -> a bigger map, never shrinking.
        let (mut pw, mut ph) = (0u16, 0u16);
        for n in 2..=8 {
            let (w, h) = map_size_for_players(n, 999, 999);
            assert!(w >= pw && h >= ph, "{n} players shrank the map");
            assert!(w % 2 == 1 && h % 2 == 1, "dimensions must be odd: {w}x{h}");
            pw = w;
            ph = h;
        }
        // Two players is small; a full lobby is a bit above the classic size.
        assert_eq!(map_size_for_players(2, 999, 999), (15, 13));
        assert_eq!(map_size_for_players(8, 999, 999), (27, 25));
    }

    #[test]
    fn map_size_respects_terminal_cap_and_minimum() {
        // Never larger than the host's terminal-derived cap...
        let (w, h) = map_size_for_players(8, 19, 17);
        assert!(w <= 19 && h <= 17);
        assert!(w % 2 == 1 && h % 2 == 1);
        // ...but never below the playable floor, and the count clamps at 2 and 8.
        let (w, h) = map_size_for_players(1, 999, 999);
        assert!(w >= 13 && h >= 11);
        assert_eq!(map_size_for_players(99, 999, 999), map_size_for_players(8, 999, 999));
    }

    #[test]
    fn spawns_are_walkable_on_the_smallest_map() {
        // The 2-player map must still leave every spawn it uses walkable.
        let (w, h) = map_size_for_players(2, 999, 999);
        let map = Map::generate(w, h);
        for id in 0..2 {
            let (sx, sy) = spawn_pos(id, w, h);
            assert!(map.is_walkable(sx, sy), "spawn {id} blocked on {w}x{h} map");
        }
    }
}
