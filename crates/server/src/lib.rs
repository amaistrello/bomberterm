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
use common::types::{Player, PlayerId, Bomb, Explosion, Direction};
use tracing::{info, warn, error};

type FramedStream = tokio_serde::Framed<tokio_util::codec::Framed<TcpStream, LengthDelimitedCodec>, ClientMsg, ServerMsg, Bincode<ClientMsg, ServerMsg>>;

pub struct ServerConfig {
    pub port: u16,
    pub max_players: u8,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self { port: 7777, max_players: 4 }
    }
}

struct SharedState {
    next_player_id: PlayerId,
    map: Map,
    players: HashMap<PlayerId, Player>,
    bombs: Vec<Bomb>,
    explosions: Vec<Explosion>,
    tick: u64,
    phase: GamePhase,
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
            phase: GamePhase::Lobby,
        }
    }

    fn add_player(&mut self, name: String) -> PlayerId {
        let id = self.next_player_id;
        self.next_player_id += 1;
        let spawn = spawn_pos(id);
        self.players.insert(id, Player::new(id, name, spawn));
        if self.players.len() >= 2 && self.phase == GamePhase::Lobby {
            self.phase = GamePhase::Running;
            info!("Game started!");
        }
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
}

fn spawn_pos(id: PlayerId) -> (u16, u16) {
    match id % 4 {
        0 => (1,  1),
        1 => (13, 11),
        2 => (1,  11),
        _ => (13, 1),
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

    let state = Arc::new(Mutex::new(SharedState::new()));
    let (snapshot_tx, _) = broadcast::channel::<GameSnapshot>(16);
    let (input_tx, input_rx) = mpsc::channel::<PlayerInput>(64);

    tokio::spawn(game_loop(state.clone(), snapshot_tx.clone(), input_rx));

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

                if let Some((nx, ny)) = new_pos {
                    let bomb_there = s.bombs.iter().any(|b| b.pos == (nx, ny));
                    if s.map.is_walkable(nx, ny) && !bomb_there {
                        if let Some(player) = s.players.get_mut(&input.player_id) {
                            player.pos = (nx, ny);
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

            // 3. Explode
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
                s.explosions.push(Explosion { cells, ttl: 5 });
            }

            // 4. Kill players
            let explosion_cells: Vec<(u16, u16)> = s.explosions
                .iter().flat_map(|e| e.cells.iter().copied()).collect();
            for player in s.players.values_mut() {
                if player.alive && explosion_cells.contains(&player.pos) {
                    player.alive = false;
                    info!("Player {} killed", player.id);
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
                    s.map = Map::generate(15, 13);
                    s.bombs.clear();
                    s.explosions.clear();
                    s.tick = 0;
                    let ids: Vec<PlayerId> = s.players.keys().copied().collect();
                    for (i, id) in ids.iter().enumerate() {
                        if let Some(p) = s.players.get_mut(id) {
                            p.alive = true;
                            p.pos = spawn_pos(i as PlayerId);
                            p.bombs_placed = 0;
                        }
                    }
                    s.phase = GamePhase::Running;
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