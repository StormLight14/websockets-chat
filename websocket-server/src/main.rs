use std::{
    collections::HashMap,
    env,
    fs::OpenOptions,
    io::prelude::*,
    io::Error as IoError,
    net::SocketAddr,
    sync::{Arc, Mutex},
};

use futures_channel::mpsc::{unbounded, UnboundedSender};
use futures_util::{future, pin_mut, stream::TryStreamExt, StreamExt};

use tokio::net::{TcpListener, TcpStream};
use tokio_tungstenite::tungstenite::protocol::Message;

type Tx = UnboundedSender<Message>;
type PeerMap = Arc<Mutex<HashMap<SocketAddr, Tx>>>;

async fn handle_connection(peer_map: PeerMap, raw_stream: TcpStream, addr: SocketAddr, users: Arc<Mutex<Vec<(SocketAddr, String)>>>) {
    //println!("Incoming TCP connection from: {}", addr);
    let mut server_msgs: Vec<String> = Vec::new();
    let ws_stream = tokio_tungstenite::accept_async(raw_stream)
        .await
        .expect("Error during the websocket handshake occurred");
    println!("WebSocket connection established: {}", addr);

    // Insert the write part of this peer to the peer map.
    let (tx, rx) = unbounded();
    peer_map.lock().unwrap().insert(addr, tx);

    let (outgoing, incoming) = ws_stream.split();

    let broadcast_incoming = incoming.try_for_each(|msg| {
        let msg_text = msg.to_text().unwrap();
        if msg_text.starts_with("[JOIN_ATTEMPT]") {
            let joined_username = msg_text.replace("[JOIN_ATTEMPT] ", "");
            let mut can_join = true;
            let mut users = users.lock().unwrap();
            for (_user_addr, username) in users.iter() {
                if &joined_username == username {
                    can_join = false;
                    println!("A user tried to join with the username {}, but that was already taken.", username);
                    if let Some(tx) = peer_map.lock().unwrap().get(&addr) {
                        tx.unbounded_send(Message::text("[SERVER_RESPONSE] That username is already taken.")).unwrap();
                    }
                    peer_map.lock().unwrap().remove(&addr);
                }
            }

            if can_join {
                println!("User {} joined from {}.", joined_username, addr);
                users.push((addr, joined_username.clone()));
                server_msgs.push(format!("[SERVER]: User {} has joined.", joined_username));
            }
        } else {
            println!("Received a message from {}: {}", addr, msg.to_text().unwrap());
        }

        let mut log_file = OpenOptions::new()
            .create(true)
            .append(true)
            .open("message_log.txt")
            .unwrap();

        log_file
            .write_all(msg.to_text().unwrap().as_bytes())
            .unwrap();

        let peers = peer_map.lock().unwrap();

        // We want to broadcast the message to everyone except ourselves.
        let broadcast_recipients = peers
            .iter()
            .filter(|(peer_addr, _)| peer_addr != &&addr)
            .map(|(_, ws_sink)| ws_sink);

        for recp in broadcast_recipients {
            println!("{:?}", server_msgs);
            for message in server_msgs.iter() {
                recp.unbounded_send(Message::text(message)).unwrap();
                println!("Server sent out: {}", message);
            }
            recp.unbounded_send(msg.clone()).unwrap();
        }

        future::ok(())
    });

    let receive_from_others = rx.map(Ok).forward(outgoing);

    pin_mut!(broadcast_incoming, receive_from_others);
    future::select(broadcast_incoming, receive_from_others).await;
    
    let mut users = users.lock().unwrap();
    users.retain(|(address, _)| *address != addr); // remove all users of the same address 
    println!("{} disconnected", &addr);
    peer_map.lock().unwrap().remove(&addr);
}

#[tokio::main]
async fn main() -> Result<(), IoError> {
    let addr = env::args()
        .nth(1)
        .unwrap_or_else(|| "127.0.0.1:8080".to_string());

    let state = PeerMap::new(Mutex::new(HashMap::new()));
    let users = Arc::new(Mutex::new(Vec::new()));

    // Create the event loop and TCP listener we'll accept connections on.
    let try_socket = TcpListener::bind(&addr).await;
    let listener = try_socket.expect("Failed to bind");
    println!("Listening on: {}", addr);

    // Let's spawn the handling of each connection in a separate task.
    while let Ok((stream, addr)) = listener.accept().await {
        tokio::spawn(handle_connection(state.clone(), stream, addr, users.clone()));
    }

    Ok(())
}
