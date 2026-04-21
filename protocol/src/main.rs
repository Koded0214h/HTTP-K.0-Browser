#![allow(dead_code)]
// src/main.rs — HTTP/K.0 protocol runner

// Phase 1 — transport
mod ack;
mod congestion;
mod connection;
mod fec;
mod packet;
mod transport;

// Phase 2 — protocol
mod tls;
mod frame;
mod hpack;
mod flow;
mod session;

use std::net::SocketAddr;
use std::time::Duration;
use transport::K0Transport;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();

    match args.get(1).map(|s| s.as_str()) {

        Some("server") => {
            println!("=== HTTP/K.0 Server ===");
            let t = K0Transport::bind("0.0.0.0:9000").await?;
            t.run().await?;
        }

        Some("client") => {
            println!("=== HTTP/K.0 Client ===");
            let peer: SocketAddr = args
                .get(2)
                .and_then(|s| s.parse().ok())
                .unwrap_or("127.0.0.1:9000".parse()?);

            let conn_id = 0x4b30_dead_cafe_beef_u64;
            let t = K0Transport::bind("0.0.0.0:0").await?;
            t.connect(peer, conn_id).await?;

            for i in 0..5u32 {
                let msg = format!("reliable message #{i}");
                t.send_stream(conn_id, 0, msg.as_bytes()).await?;
                tokio::time::sleep(Duration::from_millis(50)).await;
            }

            for i in 0..8u32 {
                let msg = format!("game state update #{i}");
                t.send_stream(conn_id, 1, msg.as_bytes()).await?;
                tokio::time::sleep(Duration::from_millis(16)).await;
            }

            // in the client match arm, replace the stats block:
            if let Some(stats) = t.stats(conn_id).await {
                println!("\n[K0] Connection stats:");
                println!("  SRTT:       {}ms", stats.srtt_ms);
                println!("  RTO:        {}ms", stats.rto_ms);
                println!("  CWND:       {} packets", stats.cwnd);
                println!("  In-flight:  {}", stats.in_flight);
                println!("  Min RTT:    {}ms", stats.min_rtt_ms);
                println!("  Max BW:     {:.1} kbps", stats.max_bw_kbps);
                println!("  BBR phase:  {}", stats.phase);
                println!("  Encrypted:  {}", stats.encrypted);
                println!("  0-RTT:      {}", stats.zero_rtt);
            }

            t.close(conn_id).await?;
        }

        Some("bench") => {
            println!("=== HTTP/K.0 Loopback Bench ===");
            let server = K0Transport::bind("127.0.0.1:9100").await?;
            let client = K0Transport::bind("127.0.0.1:0").await?;
            let peer: SocketAddr = "127.0.0.1:9100".parse()?;
            let conn_id = 0xbe_0000_0000_0001_u64;

            tokio::spawn(async move { server.run().await.unwrap(); });
            tokio::time::sleep(Duration::from_millis(50)).await;
            client.connect(peer, conn_id).await?;

            let start   = std::time::Instant::now();
            let n       = 1000usize;
            let payload = vec![0x42u8; 1000];
            for _ in 0..n {
                client.send_stream(conn_id, 0, &payload).await?;
            }
            let elapsed = start.elapsed();
            let mbps = (n * 1000 * 8) as f64 / elapsed.as_secs_f64() / 1_000_000.0;
            println!("[bench] {} pkts in {:.2}s → {:.2} Mbps", n, elapsed.as_secs_f64(), mbps);
            client.close(conn_id).await?;
        }

        // Demo: show Phase 2 modules working
        Some("demo") => {
            println!("=== HTTP/K.0 Phase 2 Demo ===\n");

            // HPACK compression
            println!("── HPACK compression ──");
            let dec = hpack::HpackDecoder::new();
            let req = hpack::K0Request::get("/api/users");
            let encoded = req.encode_headers();
            let decoded = dec.decode(&encoded).unwrap();
            println!("Request headers encoded: {} bytes", encoded.len());
            for h in &decoded { println!("  {}:{}", h.name, h.value); }

            // HTTP frame
            println!("\n── Frame encoding ──");
            let frame = frame::K0Frame::data(b"Hello from K.0!".as_ref());
            let encoded_frame = frame.encode();
            println!("DATA frame: {} bytes (5B header + {}B payload)",
                encoded_frame.len(), b"Hello from K.0!".len());

            let mut dec2 = frame::FrameDecoder::new();
            dec2.feed(&encoded_frame);
            let frames = dec2.drain_frames();
            println!("Decoded: {:?} frames", frames.len());

            // Flow control
            println!("\n── Flow control ──");
            let mut stream_fc = flow::StreamFlowControl::new(0);
            let mut conn_fc   = flow::ConnectionFlowControl::new();
            println!("Initial send window: {} bytes", stream_fc.send.available);
            stream_fc.consume_send(1024, &mut conn_fc);
            println!("After sending 1KB:   {} bytes remaining", stream_fc.send.available);

            // Session store
            println!("\n── 0-RTT session store ──");
            let store = session::SessionStore::new();
            println!("Can resume k0.dev? {}", store.can_resume("k0.dev"));
            // (would store ticket after real handshake)
            println!("GET safe for 0-RTT? {}", session::is_safe_for_zero_rtt("GET"));
            println!("POST safe for 0-RTT? {}", session::is_safe_for_zero_rtt("POST"));

            // Packet crypto
            println!("\n── Packet encryption (ChaCha20-Poly1305) ──");
            let key = tls::PacketCrypto::random_key().unwrap();
            let enc_crypto = tls::PacketCrypto::new(&key).unwrap();
            let dec_crypto = tls::PacketCrypto::new(&key).unwrap();
            let plaintext  = b"GET /index.html HTTP/K.0";
            let aad        = b"\x00\x00\x00\x00\x00\x00\x00\x01\x00\x10";
            let mut ct     = enc_crypto.encrypt(1, aad, plaintext).unwrap();
            println!("Encrypted {} bytes → {} bytes (+ 16B auth tag)", plaintext.len(), ct.len());
            let recovered  = dec_crypto.decrypt(1, aad, &mut ct).unwrap();
            println!("Decrypted: \"{}\"", std::str::from_utf8(recovered).unwrap());
        }

        _ => {
            eprintln!("Usage: http-k0 <server|client [addr]|bench|demo>");
        }
    }
    Ok(())
}