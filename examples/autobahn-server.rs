use futures::StreamExt;
use log::*;
use async_std::net::{TcpListener, TcpStream, SocketAddr, ToSocketAddrs};
use async_tungstenite::accept_async;

async fn accept_connection(peer: SocketAddr, stream: TcpStream) {
    let mut ws_stream = accept_async(stream).await.expect("Failed to accept");

    info!("New WebSocket connection: {}", peer);

    while let Some(msg) = ws_stream.next().await {
        let msg = msg.expect("Failed to get request");
        if msg.is_text() || msg.is_binary() {
            ws_stream.send(msg).await.expect("Failed to send response");
        }
    }
}

async fn run() {
    env_logger::init();

    let addr = "127.0.0.1:9002"
        .to_socket_addrs()
        .await
        .expect("Not a valid address")
        .next()
        .expect("Not a socket address");
    let socket = TcpListener::bind(&addr).await.unwrap();
    let mut incoming = socket.incoming();
    info!("Listening on: {}", addr);

    while let Some(stream) = incoming.next().await {
        let stream = stream.expect("Failed to get stream");
        let peer = stream
            .peer_addr()
            .expect("connected streams should have a peer address");
        info!("Peer address: {}", peer);

        async_std::task::spawn(accept_connection(peer, stream));
    }
}

fn main() {
    async_std::task::block_on(run());
}
