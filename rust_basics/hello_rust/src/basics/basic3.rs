fn parse_url(url: &str) -> Result<&str, &str> {
    if(url.starts_with("k0://")) {
        Ok(url)
    } else {
        Err("Invalid scheme")
    }
}

match parse_url("k0://game.example.com") {
    Ok(u) => println!("Parsed URL: {}", u),
    Err(e) => println!("Error: {}", e),
}

fn validate_url(url: &str) -> Result<&str, &str> {
    if url.starts_with("k0://") {
        Ok(url)
    } else {
        Err("Invalid scheme")
    }
}

fn build_request(url: &str) -> Result<BrowserRequest, &str> {
    let valid_url = validate_url(url)?;  // If Err, return immediately cause of "?"
    Ok(BrowserRequest {
        url: valid_url.to_string(),
        method: "GET".to_string(),
        headers: vec![],
    })
}

pub fn basic3() {
    // Error Handling:


    let username: Option<String> = Some("koded".to_string());
    let missing: Option<String> = None;
}