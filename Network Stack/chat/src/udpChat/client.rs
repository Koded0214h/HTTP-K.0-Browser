use std::io::{self};
use std::net::UdpSocket;
use std::thread;

fn main() -> std::io::Result<()> {
    let socket = UdpSocket::bind("127.0.0.1:0")?; // OS picks ephemeral port
    socket.connect("127.0.0.1:9000")?; // set default send/recv peer

    // Spawn thread to receive messages
    let recv = socket.try_clone()?;
    thread::spawn(move || {
        let mut buf = [0u8; 1500];
        loop {
            match recv.recv_from(&mut buf) {
                Ok((amt, src)) => {
                    println!("From {}: {}", src, String::from_utf8_lossy(&buf[..amt]));
                }
                Err(e) => {
                    eprintln!("Recv error: {}", e);
                    break;
                }
            }
        }
    });

    // Main thread sends user input to server
    let mut input = String::new();
    loop {
        input.clear();
        io::stdin().read_line(&mut input)?;
        if input.trim().is_empty() { continue; }
        socket.send(input.as_bytes())?;
    }
}
