use std::collections::HashSet;
use std::net::UdpSocket;

fn main() -> std::io::Result<()> {
    let socket = UdpSocket::bind("127.0.0.1:9000")?;
    println!("UDP chat server listening on 127.0.0.1:9000");

    let mut buf = [0u8; 1500];
    let mut peers: HashSet<std::net::SocketAddr> = HashSet::new();

    loop {
        let (amt, src) = socket.recv_from(&mut buf)?;
        peers.insert(src);

        let msg = &buf[..amt];
        println!("From {}: {}", src, String::from_utf8_lossy(msg));

        // rebroadcast to all known peers (including sender if desired)
        for &peer in peers.iter() {
            // ignore individual send errors
            let _ = socket.send_to(msg, peer);
        }
    }
}
