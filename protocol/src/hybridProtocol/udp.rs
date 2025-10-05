use std::net::UdpSocket;

pub fn start_udp_server() -> std::io::Result<()> {
    let socket = UdpSocket::bind("127.0.0.1:9001")?;
    println!("⚡ UDP Data Server listening on port 9001");

    let mut buf = [0u8; 1024];
    loop {
        let (amt, src) = socket.recv_from(&mut buf)?;
        let msg = String::from_utf8_lossy(&buf[..amt]);
        println!("📥 From {}: {}", src, msg);

        // Echo message back
        socket.send_to(&buf[..amt], src)?;
    }
}
