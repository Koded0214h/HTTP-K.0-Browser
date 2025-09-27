mod sockets;

fn main() {
    println!("TCP Without Headers!");
    sockets::tcp::run();

    println!("\nTCP With Headers!");
    sockets::tcp_with_headers::run();

    println!("\nUDP!");
    sockets::udp::run();
}
