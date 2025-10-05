mod hybridProtocol;

use hybridProtocol::{tcp::start_tcp_server, udp::start_udp_server, client::run_client};

fn main() -> std::io::Result<()> {
    // Run each in a separate terminal:
    // start_tcp_server()  → Control channel
    // start_udp_server()  → Data channel
    // run_client()        → Hybrid client

    // Uncomment what you want to run:
    // start_tcp_server()?;
    // start_udp_server()?;
    run_client()?;

    Ok(())
}
