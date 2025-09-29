use std::io::{Read, Write};
use std::net::TcpStream;

pub fn minimal_client() {
    let mut stream = TcpStream::connect("127.0.0.1:7878").expect("Failed to connect!");

    println!("Connected to server {:?}", stream.peer_addr().unwrap());

    stream.write_all(b"Hello from client!").unwrap();

    let mut buffer = [0; 512];
    let n = stream.read(&mut buffer).unwrap();

    println!("Response: {}", String::from_utf8_lossy(&buffer[..n]));
}


fn main() {
    minimal_client();
}

// Can be run with cargo run --bin client