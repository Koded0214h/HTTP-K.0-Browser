pub fn basic() {
    println!("Welcome to HTTP/K.0 Browser!");

    let mut url = "k0://example.com"; // Mutable can be changed
    let uri = "k0://192.168.0.7"; // Immutable cannot be changed

    println!("URL: {}", url);
    println!("URI: {}", uri);

    url = "k0://koded.io";
    // uri = "k0://192.168.0.7/koded.io"; Error cannot assign twice to immutable variable

    println!("URL: {}", url);
}


