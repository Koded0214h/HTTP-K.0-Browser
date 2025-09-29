use std::net::UdpSocket;

#[derive(Debug)]
pub struct Frame {
    pub version: u8,
    pub packet_type: u8,
    pub id: u16,
    pub length: u16,
    pub payload: Vec<u8>
}

impl Frame {
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut bytes = Vec::new();
        bytes.push(self.version);
        bytes.push(self.packet_type);
        bytes.extend(&self.id.to_be_bytes());     // 2 bytes
        bytes.extend(&self.length.to_be_bytes()); // 2 bytes
        bytes.extend(&self.payload);              // N bytes
        bytes
    }

    pub fn from_bytes(data: &[u8]) -> Option<Self> {
        if data.len() < 6 { return None; } // header = 1+1+2+2 = 6 bytes

        let version = data[0];
        let packet_type = data[1];
        let id = u16::from_be_bytes([data[2], data[3]]);
        let length = u16::from_be_bytes([data[4], data[5]]);

        if data.len() < 6 + length as usize { return None; }

        let payload = data[6..6 + length as usize].to_vec();

        Some(Frame {
            version,
            packet_type,
            id,
            length,
            payload,
        })
    }
}


pub fn run() {
    let socket = UdpSocket::bind("127.0.0.1:8000").expect("Couldn't bind to address");

    socket.send_to(b"Hello UDP!", "127.0.0.1:8000").expect("Couldn't send data");

    let mut buff = [0; 1024];
    let (amt, src) = socket.recv_from(&mut buff).expect("Receive failed!");

    println!("received from {}: {}", src, String::from_utf8_lossy(&buff[..amt]));
}