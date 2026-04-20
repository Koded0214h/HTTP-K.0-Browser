// src/main.rs — run server or client from args

mod connection;
mod packet;
mod transport;

use transport::K0Transport;
use std::net::SocketAddr;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    match args.get(1).map(|s| s.as_str()) {
        Some("server") => {
            let t = K0Transport::bind("0.0.0.0:9000").await?;
            t.run().await?;
        }
        Some("client") => {
            let peer: SocketAddr = "127.0.0.1:9000".parse()?;
            let t = K0Transport::bind("0.0.0.0:0").await?;
            t.connect(peer, 0x4f2a9b1e_deadbeef).await?;
        }
        _ => eprintln!("Usage: http-k0 [server|client]"),
    }
    Ok(())
}