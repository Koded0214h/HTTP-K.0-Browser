use std::io::{Read, Write};
use std::net::TcpListener;

pub fn server() {

    let listener = TcpListener::bind("127.0.0.1:7878").expect("Failed to bind to address");

    println!("Server listening on 127.0.0.1:7878");

    for stream in listener.incoming() {
        match stream {
            Ok(mut stream) => {
                println!("New connection from {:?}", stream.peer_addr().unwrap());

                let mut buffer = [0; 512];
                let n = stream.read(&mut buffer).unwrap();

                println!("Received: {}", String::from_utf8_lossy(&buffer[..n]));

                stream.write_all(b"Hello from server!").unwrap();
            }
            Err(e) => {
                println!("Connection failed: {}", e);
            }
        }
    }

}

fn main() {
    server();
}


// Can be run with cargo run --bin server