mod sockets;
mod tests;
use sockets::chat::{server::run_server, client::run_client};

fn main() {
    // run_server();
    run_client();
}