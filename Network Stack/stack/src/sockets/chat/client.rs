use std::io::{self, Read, Write};
use std::net::TcpStream;
use std::thread;

pub fn run_client() {
    
    let mut stream = TcpStream::connect("127.0.0.1:8000").expect("Cannot conect to server");
    println!("✅ Connected to chat server!");

    let mut stream_clone= stream.try_clone().expect("Failed to clone stream");

    thread::spawn(move || {
        let mut buffer  = [0; 1024];
        loop {
            match stream_clone.read(&mut buffer) {
                Ok(0) => break,
                Ok(n) => {
                    println!("💬 Server: {}", String::from_utf8_lossy(&buffer[..n]));
                },
                Err(_) => {
                    println!("⚠️ Conection closed");
                    break;
                }
            }
        }    
    });

    loop {
        let mut input = String::new();
        io::stdin().read_line(&mut input).unwrap();

        stream.write_all(input.as_bytes()).unwrap();
    }

}