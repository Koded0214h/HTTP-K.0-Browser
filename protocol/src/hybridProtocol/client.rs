use std::net::{TcpStream, UdpSocket};
use std::io::{Read, Write};

pub fn run_client() -> std::io::Result<()> {
    // 1️⃣ Connect via TCP (control channel)
    let mut tcp = TcpStream::connect("127.0.0.1:7000")?;
    println!("✅ Connected to TCP server");

    // Receive UDP port number
    let mut port_buf = [0u8; 2];
    tcp.read_exact(&mut port_buf)?;
    let udp_port = u16::from_be_bytes(port_buf);
    println!("📡 Server said to use UDP port {}", udp_port);

    // 2️⃣ Start UDP communication (data channel)
    let udp = UdpSocket::bind("127.0.0.1:0")?; // Bind to any free port
    udp.connect(("127.0.0.1", udp_port))?;
    println!("⚡ Connected to UDP server on port {}", udp_port);

    // Send test message via UDP
    let msg = b"Hello from hybrid client!";
    udp.send(msg)?;
    println!("📨 Sent UDP message: {}", String::from_utf8_lossy(msg));

    // Receive echo
    let mut buf = [0u8; 1024];
    let amt = udp.recv(&mut buf)?;
    println!("📬 Echo from server: {}", String::from_utf8_lossy(&buf[..amt]));

    Ok(())
}
