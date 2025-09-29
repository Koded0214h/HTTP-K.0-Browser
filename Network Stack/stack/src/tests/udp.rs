use std::net::UdpSocket;

pub fn udp_test() {
    let socket = UdpSocket::bind("127.0.0.1:9000").expect("Unable to bind port");

    socket.send_to(b"Ping!", "127.0.0.1:9000").expect("Unable to send data");

    let mut buff = [0; 1024];

    let (amt, src) = socket.recv_from(&mut buff).expect("Receive failed");

    println!("received from {}: {}", src, String::from_utf8_lossy(&buff[..amt]));
}