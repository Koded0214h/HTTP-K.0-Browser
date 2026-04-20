// src/main.rs — HTTP/K.0 protocol runner

mod ack;
mod congestion;
mod connection;
mod fec;
mod packet;
mod transport;

use std::net::SocketAddr;
use std::time::Duration;
use transport::K0Transport;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();

    match args.get(1).map(|s| s.as_str()) {

        // ── SERVER ────────────────────────────────────────────────────────────
        Some("server") => {
            println!("=== HTTP/K.0 Server ===");
            let t = K0Transport::bind("0.0.0.0:9000").await?;
            t.run().await?;
        }

        // ── CLIENT ────────────────────────────────────────────────────────────
        Some("client") => {
            println!("=== HTTP/K.0 Client ===");
            let peer: SocketAddr = args
                .get(2)
                .and_then(|s| s.parse().ok())
                .unwrap_or("127.0.0.1:9000".parse()?);

            let conn_id = 0x4b30_dead_cafe_beef_u64; // "K0" in ASCII at front
            let t = K0Transport::bind("0.0.0.0:0").await?;

            // 3-way handshake
            t.connect(peer, conn_id).await?;

            // send on reliable stream (id=0)
            for i in 0..5u32 {
                let msg = format!("reliable message #{i}");
                t.send_stream(conn_id, 0, msg.as_bytes()).await?;
                tokio::time::sleep(Duration::from_millis(50)).await;
            }

            // send on unreliable stream (id=1) — FEC kicks in every 4 packets
            for i in 0..8u32 {
                let msg = format!("game state update #{i}");
                t.send_stream(conn_id, 1, msg.as_bytes()).await?;
                tokio::time::sleep(Duration::from_millis(16)).await; // ~60fps cadence
            }

            // print stats
            if let Some(stats) = t.stats(conn_id).await {
                println!("\n[K0] Connection stats:");
                println!("  SRTT:       {}ms", stats.srtt_ms);
                println!("  RTO:        {}ms", stats.rto_ms);
                println!("  CWND:       {} packets", stats.cwnd);
                println!("  In-flight:  {}", stats.in_flight);
                println!("  Min RTT:    {}ms", stats.min_rtt_ms);
                println!("  Max BW:     {:.1} kbps", stats.max_bw_kbps);
                println!("  BBR phase:  {}", stats.phase);
            }

            // graceful close
            t.close(conn_id).await?;
        }

        // ── BENCH (loopback stress test) ──────────────────────────────────────
        Some("bench") => {
            println!("=== HTTP/K.0 Loopback Bench ===");
            let server = K0Transport::bind("127.0.0.1:9100").await?;
            let client = K0Transport::bind("127.0.0.1:0").await?;
            let peer: SocketAddr = "127.0.0.1:9100".parse()?;
            let conn_id = 0xbe_0000_0000_0001_u64;

            tokio::spawn(async move {
                server.run().await.unwrap();
            });

            tokio::time::sleep(Duration::from_millis(50)).await;
            client.connect(peer, conn_id).await?;

            let start = std::time::Instant::now();
            let n = 1000usize;
            let payload = vec![0x42u8; 1000]; // 1KB packets
            for _ in 0..n {
                client.send_stream(conn_id, 0, &payload).await?;
            }
            let elapsed = start.elapsed();
            let throughput_mbps =
                (n * 1000 * 8) as f64 / elapsed.as_secs_f64() / 1_000_000.0;
            println!(
                "[bench] {} packets in {:.2}s → {:.2} Mbps",
                n,
                elapsed.as_secs_f64(),
                throughput_mbps
            );

            client.close(conn_id).await?;
        }

        _ => {
            eprintln!("Usage: http-k0 <server|client [peer_addr]|bench>");
        }
    }

    Ok(())
}