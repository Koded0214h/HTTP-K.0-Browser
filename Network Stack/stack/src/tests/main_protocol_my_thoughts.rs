use std::net::TcpStream;
use std::net::UdpSocket;

// If i choose to use TCP to send(login/authentication)
// And i use UDP to receive for faster UX while gaming...
// Server: 127.0.0.1:8000 to send data 80 for HTTP and 443 for HTTPS
// Client: 127.0.0.1:9000 to receive data
// This one is based without Frame for the UDP -- I know its unconventonal

pub fn send() {
    
    let mut stream = TcpStream::connect("127.0.0.1:80").expect("Failed to connect to stream");

    let request = "GET / HTTP / 1.1\r\nHost: 127.0.01:80\r\n\r\n";

    stream.write_all(request.as_bytes()).expect("Failed to write to stream");

    let mut buff = [0; 1024];

}

pub fn receive() {

    let socket = UdpSocket::bind("127.0.0.1:9000").expect("Failed to bind port");

    let (amt, src) = socket.recv_from(&mut buff).expect("Failed to receive data");

    println!("Receieved from {}: {}", src, String::from_utf8_lossy(&mut buff[..amt]));

}

pub fn run() {
    send();
    receive();
}

// If i choose to use TCP to send(login/authentication)
// And i use UDP to receive for faster UX while gaming...
// Server: 127.0.0.1:8000 to send data 80 for HTTP and 443 for HTTPS
// Client: 127.0.0.1:9000 to receive data
// This one is based on UDP Frame

pub fn send() {

    let mut stream = TcpStream::connect("127.0.0.1:80").expect("Failed to connect to stream");

    let request = "GET / HTTP/1.1\r\nHost: 127.0.0.1:80\r\n\r\n";

    stream.write_all(request.as_bytes()).expect("Failed to write to stream");

    let mut buff = [0; 1024];

}

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
        bytes.extend(&self.id.to_be_bytes());
        bytes.extend(&self.length.to_be_bytes());
        bytes.extend(self.payload);
        bytes;

    }

    pub fn from_bytes(data: &[u8]) -> Option<Self> { // the Option<Self> is just incase there is an error 
        // it should return Self?

        if data.len() < 6 { return None; }

        let version = data[0];
        let packet_type = data[1];
        let id = u16::from_be_bytes([data[2], data[3]]);
        let length = u16::from_be_bytes([data[4], data[5]]);

        if data.len() < 6 + length as usize { return None; } // what is Usize? 
        
        let payload = data[6..6 + length as usize].to_vec();

        Some(Self { version, packet_type, id, length, payload })

    }
}

pub fn receive() {
    let socket = UdpSocket::bind("127.0.0.1:9000").expect("Failed to bind port");

    // My conceptual thoughts,
    // I know i dont have the different properties of the
    // buffer fetched from the TcpStream, but maybe theres a way.
    // Curently reviewing the QUIC protocol documentation

    let frame =  Frame {
        version: buff.version,
        packet_type: buff.packet_type,
        id: buff.id,
        length: buff.length,
        payload: b"ping".to_vec(),
    };

    // or wait i just realised that Frame is used 
    // to create the packet for the UdpSocket.
    // and i want to fetch the buffer from the TcpStream so i dont need frame..
    // or do I?

    let (amt, src) = socket.recv_from(&mut buff).expect("Failed to receive payload");

    if let Some(received) = Frame::from_bytes(&mut buff[..amt]) {
        println!("received {:?} from {}", received, src);
    } else {
        println!("Failed to parse frame");
    }

}

pub fn run () {

    send();
    receive();

}

// Maybe i should have used `impl` 
// for the TcpStream and UdpSocket instead of all this rubbish i did