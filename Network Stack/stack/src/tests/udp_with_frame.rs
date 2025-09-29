use std::net::UdpSocket;

#[derive(Debug)]
pub struct Frame {
    pub version: u8,
    pub packet_type: u8,
    pub id: u16,
    pub length: u16,
    pub payload: Vec<u8>,
}

impl Frame {
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut bytes = Vec::new();
        bytes.push(self.version);
        bytes.push(self.packet_type);
        bytes.extend(&self.id.to_be_bytes());
        bytes.extend(&self.length.to_be_bytes());
        bytes.extend(&self.payload);
        bytes
    }

    pub fn from_bytes(data: &[u8]) -> Option<Self> {
        if data.len() < 6 { return None; }

        let version = data[0];
        let packet_type = data[1];
        let id = u16::from_be_bytes([data[2], data[3]]);
        let length = u16::from_be_bytes([data[4], data[5]]);
        if data.len() < 6 + length as usize { return None; }

        let payload = data[6..6 + length as usize].to_vec();
        Some(Self { version, packet_type, id, length, payload })
    }
}

pub fn udp_frame_test() {
    let socket = UdpSocket::bind("127.0.0.1:9001").expect("Couldn't bind socket");

    // Construct a frame
    let frame = Frame {
        version: 1,
        packet_type: 1, // maybe "ping"
        id: 42,
        length: 4,
        payload: b"pong".to_vec(),
    };

    // Send frame
    socket.send_to(&frame.to_bytes(), "127.0.0.1:9001").expect("Send failed");

    // Receive response
    let mut buff = [0; 1024];
    let (amt, src) = socket.recv_from(&mut buff).expect("Receive failed");

    if let Some(received) = Frame::from_bytes(&buff[..amt]) {
        println!("Received {:?} from {}", received, src);
    } else {
        println!("Failed to parse frame");
    }
}
