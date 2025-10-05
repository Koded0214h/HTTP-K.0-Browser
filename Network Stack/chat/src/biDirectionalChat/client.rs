use std::io::{self, Read, Write};
use std::net::TcpStream;
use std::thread;

fn main() -> std::io::Result<()> {
    let mut stream = TcpStream::connect("127.0.0.1:8000")?;
    println!("Connected to server at 127.0.0.1:8000");

    // Thread to receive messages
    let mut recv_stream = stream.try_clone()?;
    thread::spawn(move || {
        let mut buf = [0u8; 512];
        loop {
            match recv_stream.read(&mut buf) {
                Ok(0) => {
                    println!("Server closed connection.");
                    break;
                }
                Ok(n) => {
                    print!("> {}", String::from_utf8_lossy(&buf[..n]));
                }
                Err(e) => {
                    eprintln!("Read error: {}", e);
                    break;
                }
            }
        }
    });

    // Main thread to send messages
    let mut input = String::new();
    loop {
        input.clear();
        let bytes = io::stdin().read_line(&mut input)?;
        if bytes == 0 { break; } // EOF
        if input.trim().is_empty() { continue; }
        stream.write_all(input.as_bytes())?;
    }

    Ok(())
}
