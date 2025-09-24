// Allow cloning and easy debug printing
#[derive(Clone, Debug)]
pub struct Header {
    pub name: String,
    pub value: String,
}

#[derive(Debug)]
pub struct BrowserRequest {
    pub url: String,
    pub method: String,
    pub headers: Vec<Header>,
}

#[derive(Debug)]
pub struct Packet {
    pub conn_id: u64,
    pub data: Vec<u8>,
}

#[derive(Debug)]
pub enum Frame {
    Stream { id: u32, data: Vec<u8> },
    Ack { number: u64 },
    Control { info: String },
}

impl BrowserRequest {
    // Constructor that validates the URL
    pub fn new(url: &str, method: &str, headers: Vec<Header>) -> Result<Self, &'static str> {
        if !url.starts_with("k0://") {
            return Err("Invalid URL scheme: must start with k0://");
        }

        Ok(BrowserRequest {
            url: url.to_string(),
            method: method.to_string(),
            headers,
        })
    }
}

// Function to test building request, frame, and packet
pub fn test_request() {
    let headers = vec![
        Header {
            name: "Content-Type".to_string(),
            value: "application/json".to_string(),
        },
        Header {
            name: "User-Agent".to_string(),
            value: "HTTP/K.0 Browser".to_string(),
        },
    ];

    // Create a validated request
    let req = BrowserRequest::new("k0://example.com", "GET", headers.clone())
        .expect("Failed to create request");

    println!("--- Browser Request ---");
    println!("URL: {}", req.url);
    println!("Method: {}", req.method);
    for h in &req.headers {
        println!("Header: {} -> {}", h.name, h.value);
    }

    // Serialize request into bytes
    let serialized_request = format!(
        "{} {} {:?}",
        req.method,
        req.url,
        req.headers.iter().map(|h| (&h.name, &h.value)).collect::<Vec<_>>()
    )
    .into_bytes();

    // Wrap in STREAM frame
    let frame = Frame::Stream {
        id: 1,
        data: serialized_request.clone(),
    };

    // Wrap in Packet
    let packet = Packet {
        conn_id: 1234,
        data: serialized_request,
    };

    // Print Frame info
    println!("\n--- Frame ---");
    match &frame {
        Frame::Stream { id, data } => {
            println!("Stream Frame ID: {}", id);
            println!("Data bytes length: {}", data.len());
        }
        Frame::Ack { number } => println!("Frame number: {}", number),
        Frame::Control { info } => println!("Control Frame Info: {}", info),
    }

    // Print Packet info
    println!("\n--- Packet ---");
    println!("Packet conn_id: {}", packet.conn_id);
    println!("Packet data length: {}", packet.data.len());
}

fn main() {
    let headers = vec![Header {
        name: "User-Agent".to_string(),
        value: "HTTP/K.0 Browser".to_string(),
    }];

    // Test valid URL
    match BrowserRequest::new("k0://example.com", "GET", headers.clone()) {
        Ok(req) => println!("Created request to {}", req.url),
        Err(e) => println!("Error: {}", e),
    }

    // Test invalid URL
    match BrowserRequest::new("http://example.com", "GET", headers) {
        Ok(req) => println!("Created request to {}", req.url),
        Err(e) => println!("Error: {}", e),
    }

    // Run full test
    test_request();
}
