use std::net::SocketAddr;
use tokio::net::{TcpListener, TcpStream};
use tokio_util::codec::LengthDelimitedCodec;
use tokio_serde::formats::Bincode;
use futures::{SinkExt, StreamExt};
use common::protocol::{ClientMsg, ServerMsg};
use common::map::Map;
use tracing::{info, warn, error};

// This is a type alias to reduce noise.
// A `Framed` stream wraps a TcpStream with a codec that handles
// splitting the raw byte stream into discrete messages.
// Think of it as: instead of reading raw bytes, you read/write typed messages.
type FramedStream = tokio_serde::Framed<tokio_util::codec::Framed<TcpStream, LengthDelimitedCodec>, ClientMsg, ServerMsg, Bincode<ClientMsg, ServerMsg>>;

// Every async program needs an entry point that starts the tokio runtime.
// The #[tokio::main] macro sets that up for us — without it, .await wouldn't work.
#[tokio::main]
async fn main() {
    // Initialize the tracing subscriber — this makes info!/warn! etc print to stdout
    tracing_subscriber::fmt::init();

    let addr = "127.0.0.1:7777";

    // Bind the TCP listener to the address
    // This is like opening a door and saying "I'm ready to accept connections here"
    let listener = TcpListener::bind(addr).await.expect("failed to bind");
    info!("Server listening on {}", addr);

    // Accept connections in a loop — the server runs forever
    loop {
        // .accept() waits until a client connects, then returns:
        // - the TcpStream (the connection itself)
        // - the SocketAddr (the client's IP + port)
        match listener.accept().await {
            Ok((stream, addr)) => {
                info!("New connection from {}", addr);

                // Spawn a new async task for this connection.
                // This is key — each client gets its own task so they run concurrently.
                // The main loop immediately goes back to waiting for the next connection.
                tokio::spawn(async move {
                    handle_connection(stream, addr).await;
                });
            }
            Err(e) => {
                error!("Failed to accept connection: {}", e);
            }
        }
    }
}

async fn handle_connection(stream: TcpStream, addr: SocketAddr) {
    // Wrap the raw TcpStream with our framing + serialization layers.
    // This gives us a typed stream we can use with .next() and .send()
    let mut framed = make_framed(stream);

    // Wait for the first message — it must be a Join
    // StreamExt::next() returns Option<Result<ClientMsg, _>>
    // None means the connection closed, Err means a decode error
    match framed.next().await {
        Some(Ok(ClientMsg::Join { name })) => {
            info!("{} joined from {}", name, addr);

            // Send back a Welcome message
            let map = Map::generate(15, 13);
            let welcome = ServerMsg::Welcome {
                your_id: 0,        // hardcoded for now — lobby will assign real IDs
                you_are_host: true,
                map,
            };

            if let Err(e) = framed.send(welcome).await {
                error!("Failed to send Welcome to {}: {}", name, e);
                return;
            }

            info!("Sent Welcome to {}", name);

            // Keep reading messages until the client disconnects
            // Right now we just log them — game logic comes later
            while let Some(msg) = framed.next().await {
                match msg {
                    Ok(ClientMsg::Input { direction, place_bomb }) => {
                        info!(
                            "{}: input direction={:?} place_bomb={}",
                            name, direction, place_bomb
                        );
                    }
                    Ok(other) => {
                        warn!("{}: unexpected message: {:?}", name, other);
                    }
                    Err(e) => {
                        error!("{}: decode error: {}", name, e);
                        break;
                    }
                }
            }

            info!("{} disconnected", name);
        }

        Some(Ok(other)) => {
            warn!("Expected Join, got {:?} from {} — dropping connection", other, addr);
        }

        Some(Err(e)) => {
            error!("Decode error from {}: {}", addr, e);
        }

        None => {
            info!("Connection from {} closed before sending anything", addr);
        }
    }
}

// Builds the framed stream from a raw TcpStream.
// Separated into its own function because the type is complex and
// we'll reuse this exact setup on the client side too.
fn make_framed(stream: TcpStream) -> FramedStream {
    let length_delimited = tokio_util::codec::Framed::new(
        stream,
        LengthDelimitedCodec::new(),
    );

    tokio_serde::Framed::new(
        length_delimited,
        Bincode::<ClientMsg, ServerMsg>::default(),
    )
}