
pub fn build_url(protocol: &str, host: &str) -> String{
    format!("{}://{}", protocol, host)
}

pub fn print_url(url: &String) {
    println!("URL: {}", url);
}

pub fn print_urls(urls: &Vec<String>) {
    for url in urls {
        println!("{}", url);
    }
}

pub fn basic2() {
    let integer: i32 = 42;
    let float: f64 = 3.14;
    let boolean: bool = true;
    let character: char = 'K';
    let string: &str = "HTTP/K.0";

    let host: &str = "example.com";
    let protocol: &str = "k0";
    let port: i32 = 256;

    println!("URL: {}://{}:{}", protocol, host, port);

    println!("{}, {}, {}, {}, {}", integer, float, boolean, character, string);

    let url = build_url("k0", "example.com");
    println!("{}", url);


    let x = 7;

    if x < 5 {
        println!("x is small");
    } else {
        println!("x is large");
    }

    for i in 0..5 {
        println!("i = {}", i);
    } 

    let mut count = 0;
    while count < 3 {
        println!("count = {}", count);
        count += 1;
    }

    let url = String::from("k0://example.com");
    print_url(&url);
    println!("Original URL still accessible: {}", url);
}
