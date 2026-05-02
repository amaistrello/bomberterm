use tokio::net::TcpStream;
use tokio_util::codec::LengthDelimitedCodec;
use tokio_serde::formats::Bincode;
use futures::{SinkExt, StreamExt};
use common::protocol::{ClientMsg, ServerMsg};
use tracing::{info, error};

// Same framing setup as the server but types are FLIPPED:
// the client reads ServerMsg and writes ClientMsg
type FramedStream = tokio_serde::Framed<tokio_util::codec::Framed<TcpStream, LengthDelimitedCodec>, ServerMsg, ClientMsg, Bincode<ServerMsg, ClientMsg>>;

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt::init();

    let addr = "127.0.0.1:7777";

    info!("Connecting to {}", addr);

    // Connect to the server
    let stream = TcpStream::connect(addr)
        .await
        .expect("failed to connect — is the server running?");

    info!("Connected!");

    let mut framed = make_framed(stream);

    // Send Join — first thing the server expects
    let join = ClientMsg::Join { name: "Alice".to_string() };

    if let Err(e) = framed.send(join).await {
        error!("Failed to send Join: {}", e);
        return;
    }

    info!("Sent Join");

    // Wait for the server's response
    match framed.next().await {
        Some(Ok(ServerMsg::Welcome { your_id, you_are_host, map })) => {
            info!(
                "Got Welcome! id={} host={} map={}x{}",
                your_id, you_are_host, map.width, map.height
            );
            info!("Map tile at (1,1) is walkable: {}", map.is_walkable(1, 1));
        }

        Some(Ok(ServerMsg::Rejected { reason })) => {
            error!("Server rejected us: {}", reason);
            return;
        }

        Some(Ok(other)) => {
            error!("Unexpected first message: {:?}", other);
            return;
        }

        Some(Err(e)) => {
            error!("Decode error: {}", e);
            return;
        }

        None => {
            error!("Server closed connection immediately");
            return;
        }
    }

    // Send a few test inputs so we can see them appear in the server logs
    info!("Sending test inputs...");

    use common::types::Direction;

    let inputs = vec![
        ClientMsg::Input { direction: Some(Direction::Right), place_bomb: false },
        ClientMsg::Input { direction: Some(Direction::Down),  place_bomb: false },
        ClientMsg::Input { direction: None,                   place_bomb: true  },
        ClientMsg::Input { direction: Some(Direction::Left),  place_bomb: false },
    ];

    for input in inputs {
        if let Err(e) = framed.send(input).await {
            error!("Failed to send input: {}", e);
            return;
        }
        // Small delay so the server logs are readable
        tokio::time::sleep(tokio::time::Duration::from_millis(200)).await;
    }

    info!("Done. Disconnecting.");
}

fn make_framed(stream: TcpStream) -> FramedStream {
    let length_delimited = tokio_util::codec::Framed::new(
        stream,
        LengthDelimitedCodec::new(),
    );

    // Note the flip vs the server: Bincode<ServerMsg, ClientMsg>
    // read=ServerMsg, write=ClientMsg
    tokio_serde::Framed::new(
        length_delimited,
        Bincode::<ServerMsg, ClientMsg>::default(),
    )
}