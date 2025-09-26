use std::io::{Read, Write};
use std::net::TcpStream;

fn main() {
    // 1️⃣ Connect to example.com on port 80 (HTTP)
    let mut stream = TcpStream::connect("example.com:80")
        .expect("Failed to connect");

    // 2️⃣ Send a raw HTTP GET request
    let request = "GET / HTTP/1.1\r\nHost: example.com\r\n\r\n";
    stream.write_all(request.as_bytes())
        .expect("Failed to write to stream");

    // 3️⃣ Read response into buffer
    let mut buffer = [0; 2048]; // 512 bytes
    let n = stream.read(&mut buffer).expect("Failed to read");

    // 4️⃣ Print response
    println!("Response:\n{}", String::from_utf8_lossy(&buffer[..n]));
}