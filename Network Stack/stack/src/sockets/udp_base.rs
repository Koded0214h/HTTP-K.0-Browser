use std::net::UdpSocket;

fn main() {
    let socket = UdpSocket::bind("127.0.0.1:8000").expect("Couldn't bind to address");

    socket.send_to(b"Hello UDP!", "127.0.0.1:8000").expect("Couldn't send data");

    let mut buff = [0; 1024];
    let (amt, src) = socket.recv_from(&mut buff).expect("Receive failed!");

    println!("received from {}: {}", src, String::from_utf8_lossy(&buff[..amt]));
}