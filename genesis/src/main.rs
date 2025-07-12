use terminos_common::{
    block::{Block, BlockHeader, BlockVersion},
    crypto::{Address, Hashable},
    immutable::Immutable,
    serializer::Serializer,
};
use indexmap::IndexSet;

const EXTRA_NONCE_SIZE: usize = 32;

fn main() {
    // Use the developer address from configuration
    let dev_address = "tos:qsl6sj2u0gp37tr6drrq964rd4d8gnaxnezgytmt0cfltnp2wsgqqak28je";
    let address = Address::from_string(dev_address).unwrap();
    let public_key = address.to_public_key();
    
    println!("Developer address: {}", dev_address);
    println!("Developer public key: {}", public_key.to_hex());
    
    // Create genesis block header
    let header = BlockHeader::new(
        BlockVersion::V2,
        0, // height
        1752336822401, // fixed timestamp
        IndexSet::new(), // tips
        [0u8; EXTRA_NONCE_SIZE], // extra nonce
        public_key, // miner
        IndexSet::new() // transactions
    );
    
    // Create genesis block
    let block = Block::new(Immutable::Owned(header), Vec::new());
    let block_hash = block.hash();
    
    println!("\n=== New Genesis Block Information ===");
    println!("Block hex: {}", block.to_hex());
    println!("Block hash: {}", block_hash);
    println!("Block hash (bytes): {:?}", block_hash.clone().to_bytes());
    
    // Verify block
    println!("\n=== Verification Information ===");
    println!("Block version: {:?}", block.get_version());
    println!("Block height: {}", block.get_height());
    println!("Miner: {}", block.get_miner().to_hex());
    println!("Timestamp: {}", block.get_timestamp());
    println!("Transaction count: {}", block.get_transactions().len());
    
    println!("\n=== Configuration Update ===");
    println!("Please update the following content to daemon/src/config.rs:");
    println!("const MAINNET_GENESIS_BLOCK: &str = \"{}\";", block.to_hex());
    println!("const MAINNET_GENESIS_BLOCK_HASH: Hash = Hash::new({:?});", block_hash.to_bytes());
    
    // Verify generated string
    println!("\n=== Generated String Verification ===");
    println!("String length: {}", block.to_hex().len());
    println!("String content: '{}'", block.to_hex());
    
    // Test parsing
    match Block::from_hex(&block.to_hex()) {
        Ok(parsed_block) => {
            println!("✅ String can be correctly parsed as block");
            println!("Parsed block hash: {}", parsed_block.hash());
        },
        Err(e) => {
            println!("❌ String cannot be parsed as block: {:?}", e);
        }
    }
} 