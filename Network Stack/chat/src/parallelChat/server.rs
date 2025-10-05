use std::io::{Read, Write};
use std::net::{TcpStream, TcpListener};
use std::thread;

pub fn run_server() {

    let listener = TcpListener::bind("127.0.0.1:8000").expect("Unable to bind to port");

    println!("💬 Chat server running on 127.0.0.1:8000");

    for stream in listener.incoming() {
        match stream {
            Ok(mut stream) => {
                println!("New connection: {:?}", stream.peer_addr().unwrap());

                thread::spawn(move || {
                    handle_client(&mut stream);
                });
            }
            Err(e) => eprintln!("❌ Connection failed: {}", e)
        }
    }
}

pub fn handle_client(stream: &mut TcpStream) {
    let mut buffer = [0; 1024];

    loop {
        match stream.read(&mut buffer) {
            Ok(0) => {
                println!("⚠️ Client disconnected!");
                break;
            }
            Ok(n) => {
                let msg = String::from_utf8_lossy(&buffer[..n]);
                println!("📩 Received: {}", msg);

                stream.write_all(msg.as_bytes()).unwrap();
            }
            Err(_) => {
                println!("⚠️ Read error, closing connection");
                break;
            }
        }
    }
}