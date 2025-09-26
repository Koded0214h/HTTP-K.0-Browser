use std::io::{Read, Write};
use std::net::TcpStream;

pub fn run() {
    let mut stream = TcpStream::connect("example.com:80").expect("Failed to connect!");

    let request = "GET / HTTP/1.1\r\nHost: example.com\r\n\r\n";
    stream.write_all(request.as_bytes()).expect("Failed to write!");

    let mut buffer = [0; 2048]; // read up to 2KB
    let n = stream.read(&mut buffer).expect("Failed to read");

    let response = String::from_utf8_lossy(&buffer[..n]);

    if let Some((headers, body)) = response.split_once("\r\n\r\n") {
        println!("--- Headers ---\n{}\n", headers);
        println!("--- Body ---\n{}\n", &body[..200.min(body.len())]);
    } else {
        println!("Could not parse response!");
    }

}
