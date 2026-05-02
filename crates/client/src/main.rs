use tokio::net::TcpStream;
use tokio::sync::{mpsc, watch};
use tokio_util::codec::LengthDelimitedCodec;
use tokio_serde::formats::Bincode;
use futures::{SinkExt, StreamExt};
use crossterm::event::{Event, EventStream, KeyCode};
use common::map::Map;
use common::protocol::{ClientMsg, ServerMsg, GameSnapshot};
use common::types::Direction;
use tracing::{info, error};

mod tui;

type FramedStream = tokio_serde::Framed<tokio_util::codec::Framed<TcpStream, LengthDelimitedCodec>, ServerMsg, ClientMsg, Bincode<ServerMsg, ClientMsg>>;

// The state shared between the network task and the render loop.
// Clone so watch::Sender can copy it to new receivers.
#[derive(Clone)]
pub struct ClientState {
    pub map: Map,
    pub snapshot: GameSnapshot,
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt::init();

    // watch: network task writes latest state, render loop reads it
    // None = haven't received Welcome yet (still connecting)
    let (state_tx, state_rx) = watch::channel::<Option<ClientState>>(None);

    // mpsc: render loop writes inputs, network task reads + forwards them
    let (input_tx, input_rx) = mpsc::channel::<ClientMsg>(16);

    // Network task runs independently in the background
    let net = tokio::spawn(network_task(state_tx, input_rx));

    // Render loop runs on the main task — blocks until user presses Q
    render_loop(state_rx, input_tx).await;

    // Kill the network task cleanly when the render loop exits
    net.abort();
}

async fn network_task(
    state_tx: watch::Sender<Option<ClientState>>,
    mut input_rx: mpsc::Receiver<ClientMsg>,
) {
    let stream = match TcpStream::connect("127.0.0.1:7777").await {
        Ok(s) => { info!("Connected to server"); s }
        Err(e) => { error!("Failed to connect: {}", e); return; }
    };

    let mut framed = make_framed(stream);

    // Send our name — hardcoded for now, will come from the menu later
    if let Err(e) = framed.send(ClientMsg::Join { name: "Alice".to_string() }).await {
        error!("Failed to send Join: {}", e);
        return;
    }

    // First response must be Welcome — gives us the map and our player ID
    let map = match framed.next().await {
        Some(Ok(ServerMsg::Welcome { map, your_id, you_are_host })) => {
            info!("Welcome received: id={} host={}", your_id, you_are_host);
            map
        }
        other => {
            error!("Expected Welcome, got: {:?}", other);
            return;
        }
    };

    loop {
        tokio::select! {
            // Server sent something
            msg = framed.next() => {
                match msg {
                    Some(Ok(ServerMsg::StateUpdate(snapshot))) => {
                        // Push to watch channel — render loop will pick it up
                        let _ = state_tx.send(Some(ClientState {
                            map: map.clone(),
                            snapshot,
                        }));
                    }
                    Some(Ok(ServerMsg::GameOver { winner })) => {
                        info!("Game over — winner: {:?}", winner);
                        break;
                    }
                    Some(Err(e)) => { error!("Decode error: {}", e); break; }
                    None => { info!("Server disconnected"); break; }
                    _ => {}
                }
            }

            // Render loop sent an input to forward
            input = input_rx.recv() => {
                match input {
                    Some(msg) => {
                        if let Err(e) = framed.send(msg).await {
                            error!("Failed to send input: {}", e);
                            break;
                        }
                    }
                    None => break, // input channel closed = render loop exited
                }
            }
        }
    }
}

async fn render_loop(
    mut state_rx: watch::Receiver<Option<ClientState>>,
    input_tx: mpsc::Sender<ClientMsg>,
) {
    let mut terminal = tui::setup().expect("failed to setup terminal");

    // EventStream is the async version of crossterm's event polling
    let mut events = EventStream::new();

    loop {
        // Render whatever state we currently have (may be None while connecting)
        let state = state_rx.borrow().clone();
        tui::render_frame(&mut terminal, state.as_ref())
            .expect("render failed");

        tokio::select! {
            // Keyboard event arrived
            maybe_event = events.next() => {
                if let Some(Ok(Event::Key(key))) = maybe_event {
                    match key.code {
                        KeyCode::Char('q') | KeyCode::Char('Q') => break,

                        KeyCode::Up    | KeyCode::Char('w') => send_input(&input_tx, Some(Direction::Up),    false).await,
                        KeyCode::Down  | KeyCode::Char('s') => send_input(&input_tx, Some(Direction::Down),  false).await,
                        KeyCode::Left  | KeyCode::Char('a') => send_input(&input_tx, Some(Direction::Left),  false).await,
                        KeyCode::Right | KeyCode::Char('d') => send_input(&input_tx, Some(Direction::Right), false).await,
                        KeyCode::Char(' ') => send_input(&input_tx, None, true).await,
                        _ => {}
                    }
                }
            }

            // Server sent a new snapshot — loop immediately to re-render
            _ = state_rx.changed() => {}

            // Redraw every 50ms even if nothing happened (keeps the screen fresh)
            _ = tokio::time::sleep(std::time::Duration::from_millis(50)) => {}
        }
    }

    tui::teardown(&mut terminal).expect("teardown failed");
    println!("Bye!");
}

// Small helper to keep the match arms above readable
async fn send_input(tx: &mpsc::Sender<ClientMsg>, direction: Option<Direction>, place_bomb: bool) {
    let _ = tx.send(ClientMsg::Input { direction, place_bomb }).await;
}

fn make_framed(stream: TcpStream) -> FramedStream {
    let ld = tokio_util::codec::Framed::new(stream, LengthDelimitedCodec::new());
    tokio_serde::Framed::new(ld, Bincode::<ServerMsg, ClientMsg>::default())
}