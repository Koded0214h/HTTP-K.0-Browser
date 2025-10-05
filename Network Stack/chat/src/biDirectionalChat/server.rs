use std::net::{TcpListener, TcpStream};
use std::io::{Read, Write};
use std::sync::{Arc, Mutex};
use std::thread;

fn handle_client(mut stream: TcpStream, peers: Arc<Mutex<Vec<TcpStream>>>) {
    let peer_addr = stream.peer_addr().ok();
    let mut buf = [0u8; 512];

    loop {
        match stream.read(&mut buf) {
            Ok(0) => {
                eprintln!("Client {:?} disconnected", peer_addr);
                break;
            }
            Ok(n) => {
                let msg = &buf[..n];
                let msg_text = String::from_utf8_lossy(msg);
                println!("From {:?}: {}", peer_addr, msg_text);

                let mut peers_guard = peers.lock().unwrap();
                for peer in peers_guard.iter_mut() {
                    let _ = peer.write_all(msg);
                }
            }
            Err(e) => {
                eprintln!("Read error from {:?}: {}", peer_addr, e);
                break;
            }
        }
    }

    // Remove the disconnected client
    let mut peers_guard = peers.lock().unwrap();
    peers_guard.retain(|p| p.peer_addr().ok() != peer_addr);
}

fn main() -> std::io::Result<()> {
    let listener = TcpListener::bind("127.0.0.1:8000")?;
    println!("TCP chat server listening on 127.0.0.1:8000");

    let peers: Arc<Mutex<Vec<TcpStream>>> = Arc::new(Mutex::new(Vec::new()));

    for stream in listener.incoming() {
        match stream {
            Ok(stream) => {
                let peer = stream.peer_addr().ok();
                println!("New connection {:?}", peer);

                let broadcast_clone = stream.try_clone().expect("Failed to clone stream");

                {
                    let mut guard = peers.lock().unwrap();
                    guard.push(broadcast_clone);
                }

                let peers_clone = Arc::clone(&peers);
                thread::spawn(move || {
                    handle_client(stream, peers_clone);
                });
            }
            Err(e) => {
                eprintln!("Accept error: {}", e);
            }
        }
    }

    Ok(())
}
