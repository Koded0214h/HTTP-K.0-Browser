use std::io::{Read, Write};
use std::net::TcpStream;

pub fn tcp_test() {
    let mut stream = TcpStream::connect("example.com:80").expect("Failed to connect to server!");

    let request = "GET / HTTP/1.1\r\nHost: example.com\r\n\r\n";

    stream.write_all(request.as_bytes()).expect("Failed to write to stream");

    let mut buffer = [0; 100];

    let n = stream.read(&mut buff).expect("Failed to read buffer");

    println!("Response: \n{}", String::from_utf8_lossy(&buffer[..n]));
}