use terminos_common::{
    block::Block,
    serializer::Serializer,
    crypto::Hashable,
};

fn main() {
    let genesis_hex = "02000000000000000000000197ff69f08100000000000000000000000000000000000000000000000000000000000000000000000000000000000000043fa8495c7a031f2c7a68c602eaa36d5a744fa69e44822f6b7e13f5cc2a7410";
    
    println!("Genesis hex length: {}", genesis_hex.len());
    println!("Genesis hex: {}", genesis_hex);
    
    // check if the hex string is valid
    if genesis_hex.len() % 2 != 0 {
        println!("Error: Hex string length is not even!");
        return;
    }
    
    // check if the hex string only contains valid hex characters
    for (i, c) in genesis_hex.chars().enumerate() {
        if !c.is_ascii_hexdigit() {
            println!("Error: Invalid hex character '{}' at position {}", c, i);
            return;
        }
    }
    
    println!("Hex string is valid!");
    
    // try to parse the block
    match Block::from_hex(genesis_hex) {
        Ok(block) => {
            println!("Block parsed successfully!");
            println!("Block version: {:?}", block.get_version());
            println!("Block height: {}", block.get_height());
            println!("Block timestamp: {}", block.get_timestamp());
            println!("Block nonce: {}", block.get_nonce());
            println!("Block extra nonce: {}", hex::encode(block.get_extra_nonce()));
            println!("Block miner public key: {}", block.get_miner().to_hex());
            println!("Block tips count: {}", block.get_tips().len());
            println!("Block tips hash: {}", block.get_header().get_tips_hash());
            println!("Block transactions count: {}", block.get_txs_count());
            println!("Block transactions hash: {}", block.get_header().get_txs_hash());
            println!("Block work hash: {}", block.get_header().get_work_hash());
            println!("Block POW hash (V2): {:?}", block.get_pow_hash(terminos_common::block::Algorithm::V2));
            println!("Block hash: {}", block.hash());
        },
        Err(e) => {
            println!("Error parsing block: {:?}", e);
        }
    }
} 