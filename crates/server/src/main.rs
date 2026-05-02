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
    // tick() returns every 100ms — this is the heartbeat of the entire game
    let mut ticker = interval(Duration::from_millis(100));

    loop {
        ticker.tick().await;

        // Use a block so the MutexGuard is dropped before we await anything.
        // Holding a Mutex across an .await is a deadlock waiting to happen.
        let snapshot = {
            let mut s = state.lock().unwrap();
            s.tick += 1;

            while let Ok(input) = input_rx.try_recv() {
                // Phase 1: compute desired new position (immutable borrow of players)
                let new_pos = if let Some(player) = s.players.get(&input.player_id) {
                    if player.alive {
                        if let Some(dir) = input.direction {
                            let (dx, dy) = dir.delta();
                            let nx = player.pos.0 as i16 + dx;
                            let ny = player.pos.1 as i16 + dy;
                            if nx >= 0 && ny >= 0 {
                                Some((nx as u16, ny as u16))
                            } else { None }
                        } else { None }
                    } else { None }
                } else { None };
            
                // Phase 2: validate + apply (immutable map borrow, then mutable player borrow)
                if let Some((nx, ny)) = new_pos {
                    if s.map.is_walkable(nx, ny) {
                        if let Some(player) = s.players.get_mut(&input.player_id) {
                            player.pos = (nx, ny);
                        }
                    }
                }
            }

            s.snapshot()
        }; // MutexGuard dropped here

        // send() only errors if there are zero receivers — safe to ignore
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