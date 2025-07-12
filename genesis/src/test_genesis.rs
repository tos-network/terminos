fn main() {
    let genesis_hex = "02000000000000000000000197ff6669a90000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000043fa8495c7a031f2c7a68c602eaa36d5a744fa69e44822f6b7e13f5cc2a7410";
    
    println!("char string length: {}", genesis_hex.len());
    println!("char string content: '{}'", genesis_hex);
    
    // check each char
    for (i, c) in genesis_hex.chars().enumerate() {
        if !c.is_ascii_hexdigit() {
            println!("position {}: invalid char '{}' (ASCII: {})", i, c, c as u8);
        }
    }
    
    // check if the length is even
    if genesis_hex.len() % 2 != 0 {
        println!("error: string length is not even!");
    } else {
        println!("string length is even, can be parsed to byte array");
    }
    
    // try to convert to byte array
    let mut bytes = Vec::new();
    for i in (0..genesis_hex.len()).step_by(2) {
        if let Ok(byte) = u8::from_str_radix(&genesis_hex[i..i+2], 16) {
            bytes.push(byte);
        } else {
            println!("error: cannot parse byte at position {}", i);
        }
    }
    
    println!("successfully parsed {} bytes", bytes.len());
    println!("first 10 bytes: {:?}", &bytes[..std::cmp::min(10, bytes.len())]);
} 