trait Speak {
    fn speak(&self);
}

struct Dog;
struct Cat;

impl Speak for Dog {
    fn speak(&self) {
        println!("Woof!");
    }
}

impl Speak for Cat {
    fn speak(&self) {
        println!("Meow!");
    }
}

pub fn speaks() {
    let d = Dog;
    let c = Cat;

    d.speak();
    c.speak();
}

//  With generics
trait Describe {
    fn decsribe(&self) -> String;
}

struct User {
    name: string,
}
struct Product {
    id: u32,
}
impl Describe for User {
    fn decsribe(&self) -> String {
        format!("User: {}", self.name)
    }
}
impl Describe for Product {
    fn decsribe(&self) -> String {
        format!("Product ID: {}", self.id)
    }
}
fn print_description<T: Describe>(item: T) {
    println!("{}", item.describe());
}
fn main () {
    let u = User {name: "Koded".to_string() };
    let p = Product { id: 42 };

    print_description(u);
    print_description(p)
}


// -------------------------------------------
// -------------------------------------------
//         BROWSER REQUEST + TRAIT -> SENDABLE
// -------------------------------------------
// -------------------------------------------


trait Sendable {
    fn to_bytes(&self) -> Vector<u8>;
}

impl Sendable for BrowserRequest {
    fn to_bytes(&self) -> Vec<u8> {
        format!(
            "{} {} {:?}",
            self.method,
            self.url,
            self.headers.iter().map(|h| (&h.name, &h.value)).collect::<Vec<_>>()
        ).into_bytes()
    }
}

impl Sendable for Frame {
    fn to_bytes(&self) -> Vec<u8> {
        match self {
            Frame::Stream { id, data } => {
                format!("STREAM ID: {} data={}", id, data).into_bytes()
            }
            Frame::Ack { number } => format!("ACK number={}", number).into_bytes(),
            Frame::Control { info } => format!("CONTROL info={}", info).into_bytes(),
        }
    }
}

impl Sendable for Packet {
    fn to_bytes(&self) -> Vec<u8> {
        format!("Packet conn_id={} data={:?}", self.conn_id, self.data).into_bytes()
    }
}

fn send<T: Sendable>(item: &T) {
    let bytes = item.to_bytes();
    println!("Sending {} bytes: {:?}", bytes.len(), bytes);
}

// USAGE
fn main() {
    let req = BrowserRequest {
        url: "k0://example.com".to_string(),
        method: "GET".to_string(),
        headers = vec![],
    };

    let frame = Frame::Ack { number: 42 };
    let packet = Frame::Packet { conn_id: 1234, data: vec![1, 2, 3] };

    send(&req);
    send(&frame);
    send(&packet);
}