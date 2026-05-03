use std::time::{Duration, Instant};
use tokio::net::{TcpStream, UdpSocket};
use tokio::sync::{mpsc, watch};
use tokio_util::codec::LengthDelimitedCodec;
use tokio_serde::formats::Bincode;
use futures::{SinkExt, StreamExt};
use crossterm::event::{Event, EventStream, KeyCode, KeyModifiers};
use crossterm::terminal::size;
use common::protocol::{ClientMsg, ServerMsg, GameSnapshot, Beacon};
use common::types::{Direction, PlayerId};
use tracing::{info, error};

mod tui;
mod app;

use app::{Screen, NameMode, DiscoveredServer, MENU_ITEMS};

type FramedStream = tokio_serde::Framed<tokio_util::codec::Framed<TcpStream, LengthDelimitedCodec>, ServerMsg, ClientMsg, Bincode<ServerMsg, ClientMsg>>;

#[derive(Clone)]
pub struct ClientState {
    pub snapshot: GameSnapshot,
    pub your_id: PlayerId, // ← add this
}

#[tokio::main]
async fn main() {
    let file_appender = tracing_appender::rolling::never("/tmp", "bomberterm.log");
    let (non_blocking, _guard) = tracing_appender::non_blocking(file_appender);
    tracing_subscriber::fmt().with_writer(non_blocking).init();

    let mut terminal = tui::setup().expect("failed to setup terminal");
    let mut events = EventStream::new();
    let mut screen = Screen::main_menu();

    // Discovered servers — populated by the UDP listener task
    let (beacon_tx, mut beacon_rx) = mpsc::channel::<Beacon>(16);
    tokio::spawn(listen_for_beacons(beacon_tx));

    // Servers we've heard from recently
    let mut servers: Vec<DiscoveredServer> = Vec::new();

    loop {
        // Drain any new beacons into the servers list
        while let Ok(beacon) = beacon_rx.try_recv() {
            if let Some(existing) = servers.iter_mut().find(|s| s.addr == beacon.host_addr) {
                // Update existing entry
                existing.players_current = beacon.players_current;
                existing.last_seen = Instant::now();
            } else {
                servers.push(DiscoveredServer {
                    game_name: beacon.game_name,
                    addr: beacon.host_addr,
                    players_current: beacon.players_current,
                    players_max: beacon.players_max,
                    last_seen: Instant::now(),
                });
            }
        }

        // Remove servers we haven't heard from in 6 seconds
        servers.retain(|s| s.last_seen.elapsed() < Duration::from_secs(6));

        // Render
        match &screen {
            Screen::MainMenu { cursor } => {
                tui::render_main_menu(&mut terminal, *cursor).expect("render failed");
            }
            Screen::EnterName { input, mode } => {
                let hosting = matches!(mode, NameMode::Host);
                tui::render_enter_name(&mut terminal, input, hosting).expect("render failed");
            }
            Screen::ServerBrowser { cursor } => {
                tui::render_server_browser(&mut terminal, &servers, *cursor).expect("render failed");
            }
            Screen::ManualIp { input } => {
                tui::render_manual_ip(&mut terminal, input).expect("render failed");
            }
            Screen::Connecting => {
                tui::render_frame(&mut terminal, None).expect("render failed");
            }
            Screen::InGame => {}
        }

        // Input — timeout after 100ms so we redraw and drain beacons regularly
        let event = tokio::time::timeout(
            Duration::from_millis(100),
            events.next(),
        ).await;

        let key = match event {
            Ok(Some(Ok(Event::Key(k)))) => k,
            _ => continue, // timeout or non-key event — just redraw
        };

        match &mut screen {
            Screen::MainMenu { cursor } => {
                match key.code {
                    KeyCode::Up   => { if *cursor > 0 { *cursor -= 1; } }
                    KeyCode::Down => { if *cursor < MENU_ITEMS.len() - 1 { *cursor += 1; } }
                    KeyCode::Enter => match *cursor {
                        0 => screen = Screen::EnterName {
                            input: String::new(),
                            mode: NameMode::Host,
                        },
                        1 => screen = Screen::ServerBrowser { cursor: 0 },
                        2 => break,
                        _ => {}
                    },
                    KeyCode::Char('q') => break,
                    _ => {}
                }
            }

            Screen::ServerBrowser { cursor } => {
                match key.code {
                    KeyCode::Esc => { screen = Screen::main_menu(); }
                    KeyCode::Up   => { if *cursor > 0 { *cursor -= 1; } }
                    KeyCode::Down => {
                        if !servers.is_empty() && *cursor < servers.len() - 1 {
                            *cursor += 1;
                        }
                    }
                    KeyCode::Enter => {
                        if let Some(server) = servers.get(*cursor) {
                            let addr = server.addr.clone();
                            screen = Screen::EnterName {
                                input: String::new(),
                                mode: NameMode::Join { addr },
                            };
                        }
                    }
                    KeyCode::Char('m') | KeyCode::Char('M') => {
                        screen = Screen::ManualIp { input: String::new() };
                    }
                    _ => {}
                }
            }

            Screen::ManualIp { input } => {
                match key.code {
                    KeyCode::Esc => { screen = Screen::ServerBrowser { cursor: 0 }; }
                    KeyCode::Enter if !input.is_empty() => {
                        let addr = if input.contains(':') {
                            input.clone()
                        } else {
                            format!("{}:7777", input) // default port if omitted
                        };
                        screen = Screen::EnterName {
                            input: String::new(),
                            mode: NameMode::Join { addr },
                        };
                    }
                    KeyCode::Backspace => { input.pop(); }
                    KeyCode::Char(c) if input.len() < 21 => {
                        if !key.modifiers.contains(KeyModifiers::CONTROL) {
                            input.push(c);
                        }
                    }
                    _ => {}
                }
            }

            Screen::EnterName { input, mode } => {
                match key.code {
                    KeyCode::Esc => { screen = Screen::main_menu(); }
                    KeyCode::Enter if !input.is_empty() => {
                        let name = input.clone();
                        let mode = mode.clone();
                        run_game_session(&mut terminal, &mut events, name, mode).await;
                        screen = Screen::main_menu();
                    }
                    KeyCode::Backspace => { input.pop(); }
                    KeyCode::Char(c) if input.len() < 16 => {
                        if !key.modifiers.contains(KeyModifiers::CONTROL) {
                            input.push(c);
                        }
                    }
                    _ => {}
                }
            }

            Screen::Connecting | Screen::InGame => {}
        }
    }

    tui::teardown(&mut terminal).expect("teardown failed");
    println!("Bye!");
}

// Listens on the UDP discovery port and forwards beacons into a channel
async fn listen_for_beacons(tx: mpsc::Sender<Beacon>) {
    use socket2::{Socket, Domain, Type, Protocol};
    use std::net::SocketAddr;

    // Build the socket manually so we can set SO_REUSEPORT before binding
    let socket = match Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP)) {
        Ok(s) => s,
        Err(e) => { error!("UDP socket creation failed: {}", e); return; }
    };

    let _ = socket.set_reuse_address(true);
    let _ = socket.set_reuse_port(true);

    let addr: SocketAddr = format!("0.0.0.0:{}", server::DISCOVERY_PORT)
        .parse().unwrap();

    if let Err(e) = socket.bind(&addr.into()) {
        error!("UDP bind failed: {}", e);
        return;
    }

    // Convert std socket → tokio socket
    socket.set_nonblocking(true).unwrap();
    let udp: std::net::UdpSocket = socket.into();
    let socket = match UdpSocket::from_std(udp) {
        Ok(s) => s,
        Err(e) => { error!("Failed to convert UDP socket: {}", e); return; }
    };

    info!("Listening for beacons on port {}", server::DISCOVERY_PORT);

    let mut buf = vec![0u8; 1024];
    loop {
        match socket.recv_from(&mut buf).await {
            Ok((len, _addr)) => {
                if let Ok(beacon) = bincode::deserialize::<Beacon>(&buf[..len]) {
                    let _ = tx.send(beacon).await;
                }
            }
            Err(e) => { error!("UDP recv error: {}", e); break; }
        }
    }
}

async fn run_game_session(
    terminal: &mut tui::Term,
    events: &mut EventStream,
    name: String,
    mode: NameMode,
) {
    if matches!(mode, NameMode::Host) {
        info!("Starting embedded server...");
    
        // Read terminal size right now — the TUI is already running so this is accurate
        let (term_cols, term_rows) = crossterm::terminal::size().unwrap_or((80, 24));
    
        // Map panel width = total cols minus sidebar (26) minus borders (2)
        // Each tile renders as 2 chars wide so divide by 2
        let map_width = (term_cols.saturating_sub(28)) / 2;
    
        // Map height = total rows minus help bar (3) minus borders (2)
        let map_height = term_rows.saturating_sub(5);
    
        // Bomberman maps need odd dimensions so the pillar pattern works correctly
        // (hard walls sit on every even coordinate — odd dimensions ensure
        //  the border walls are always at an even+1 position)
        let map_width  = if map_width  % 2 == 0 { map_width  - 1 } else { map_width  };
        let map_height = if map_height % 2 == 0 { map_height - 1 } else { map_height };
    
        // Clamp to a sane minimum so tiny terminals don't break the game
        let map_width  = map_width.max(15);
        let map_height = map_height.max(13);
    
        info!("Map size: {}x{} (terminal {}x{})", map_width, map_height, term_cols, term_rows);
    
        tokio::spawn(server::run(server::ServerConfig {
            port: 7777,
            max_players: 8,
            map_width,
            map_height,
        }));
    
        tokio::time::sleep(tokio::time::Duration::from_millis(150)).await;
    }

    let addr = match &mode {
        NameMode::Host => "127.0.0.1:7777".to_string(),
        NameMode::Join { addr } => addr.clone(),
    };

    let stream = match TcpStream::connect(&addr).await {
        Ok(s) => s,
        Err(e) => { error!("Failed to connect to {}: {}", addr, e); return; }
    };

    let mut framed = make_framed(stream);

    if let Err(e) = framed.send(ClientMsg::Join { name }).await {
        error!("Failed to send Join: {}", e);
        return;
    }

    let (your_id, _map) = match framed.next().await {
        Some(Ok(ServerMsg::Welcome { map, your_id, you_are_host })) => {
            info!("Welcome! id={} host={}", your_id, you_are_host);
            (your_id, map)
        }
        other => { error!("Expected Welcome, got {:?}", other); return; }
    };

    let (state_tx, mut state_rx) = watch::channel::<Option<ClientState>>(None);
    let (input_tx, mut input_rx) = mpsc::channel::<ClientMsg>(16);

    let net = tokio::spawn(async move {
        loop {
            tokio::select! {
                msg = framed.next() => {
                    match msg {
                        Some(Ok(ServerMsg::StateUpdate(snapshot))) => {
                            let _ = state_tx.send(Some(ClientState { snapshot, your_id }));
                        }
                        Some(Err(e)) => { error!("Decode error: {}", e); break; }
                        None => { info!("Server disconnected"); break; }
                        _ => {}
                    }
                }
                input = input_rx.recv() => {
                    match input {
                        Some(msg) => { if framed.send(msg).await.is_err() { break; } }
                        None => break,
                    }
                }
            }
        }
    });

    loop {
        let state = state_rx.borrow().clone();
        tui::render_frame(terminal, state.as_ref()).expect("render failed");

        tokio::select! {
            maybe_event = events.next() => {
                if let Some(Ok(Event::Key(key))) = maybe_event {
                    match key.code {
                        KeyCode::Char('q') | KeyCode::Char('Q') => break,
                        KeyCode::Char('r') | KeyCode::Char('R') => {
                            let _ = input_tx.send(ClientMsg::Ready).await;
                        }
                        KeyCode::Up    | KeyCode::Char('w') => send_input(&input_tx, Some(Direction::Up),    false).await,
                        KeyCode::Down  | KeyCode::Char('s') => send_input(&input_tx, Some(Direction::Down),  false).await,
                        KeyCode::Left  | KeyCode::Char('a') => send_input(&input_tx, Some(Direction::Left),  false).await,
                        KeyCode::Right | KeyCode::Char('d') => send_input(&input_tx, Some(Direction::Right), false).await,
                        KeyCode::Char(' ')                   => send_input(&input_tx, None, true).await,
                        _ => {}
                    }
                }
            }
            _ = state_rx.changed() => {}
            _ = tokio::time::sleep(std::time::Duration::from_millis(50)) => {}
        }
    }

    net.abort();
}

async fn send_input(tx: &mpsc::Sender<ClientMsg>, direction: Option<Direction>, place_bomb: bool) {
    let _ = tx.send(ClientMsg::Input { direction, place_bomb }).await;
}

fn make_framed(stream: TcpStream) -> FramedStream {
    let ld = tokio_util::codec::Framed::new(stream, LengthDelimitedCodec::new());
    tokio_serde::Framed::new(ld, Bincode::<ServerMsg, ClientMsg>::default())
}