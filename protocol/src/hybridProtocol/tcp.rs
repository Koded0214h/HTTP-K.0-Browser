use std::net::{TcpListener, TcpStream};
use std::io::Write;

pub fn start_tcp_server() -> std::io::Result<()> {
    let listener = TcpListener::bind("127.0.0.1:7000")?;
    println!("🧠 TCP Control Server listening on port 7000");

    for stream in listener.incoming() {
        match stream {
            Ok(mut stream) => {
                let addr = stream.peer_addr()?;
                println!("📡 New TCP client: {}", addr);

                // Send UDP port number to client
                let udp_port: u16 = 9001;
                stream.write_all(&udp_port.to_be_bytes())?;
                println!("📨 Sent UDP port {} info to {}", udp_port, addr);
            }
            Err(e) => eprintln!("❌ TCP accept error: {}", e),
        }
    }
    Ok(())
}
