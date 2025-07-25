mod transaction;
mod address;
mod random;
mod block;
mod storage;
mod asset;
mod crypto;
mod memory_storage;

use bulletproofs::RangeProof;
use log::debug;
use terminos_types::{
    register_opaque_json,
    impl_opaque
};
use terminos_vm::{tid, traits::JSON_REGISTRY, OpaqueWrapper};
use crate::{
    account::CiphertextCache,
    block::Block,
    crypto::{proofs::CiphertextValidityProof, Address, Hash, Signature},
    serializer::*,
    transaction::Transaction
};
use super::ChainState;

pub use transaction::*;
pub use random::*;
pub use block::*;
pub use storage::*;
pub use address::*;
pub use asset::*;
pub use crypto::*;
pub use memory_storage::*;

// Unique IDs for opaque types serialization
pub const HASH_OPAQUE_ID: u8 = 0;
pub const ADDRESS_OPAQUE_ID: u8 = 1;
pub const SIGNATURE_OPAQUE_ID: u8 = 2;
pub const CIPHERTEXT_OPAQUE_ID: u8 = 3;
pub const CIPHERTEXT_VALIDITY_PROOF_OPAQUE_ID: u8 = 4;
pub const RANGE_PROOF_OPAQUE_ID: u8 = 5;

impl_opaque!(
    "Hash",
    Hash,
    display,
    json
);
impl_opaque!(
    "Address",
    Address,
    display,
    json
);
impl_opaque!(
    "OpaqueTransaction",
    OpaqueTransaction
);
impl_opaque!(
    "OpaqueBlock",
    OpaqueBlock
);
impl_opaque!(
    "OpaqueRandom",
    OpaqueRandom
);
impl_opaque!(
    "OpaqueStorage",
    OpaqueStorage
);
impl_opaque!(
    "OpaqueReadOnlyStorage",
    OpaqueReadOnlyStorage
);
impl_opaque!(
    "OpaqueMemoryStorage",
    OpaqueMemoryStorage
);
impl_opaque!(
    "Asset",
    Asset
);

// Injectable context data
tid!(ChainState<'_>);
tid!(Hash);
tid!(Transaction);
tid!(Block);

pub fn register_opaque_types() {
    debug!("Registering opaque types");
    let mut registry = JSON_REGISTRY.write().expect("Failed to lock JSON_REGISTRY");
    register_opaque_json!(registry, "Hash", Hash);
    register_opaque_json!(registry, "Address", Address);
    register_opaque_json!(registry, "Signature", Signature);
    register_opaque_json!(registry, "Ciphertext", CiphertextCache);
    register_opaque_json!(registry, "CiphertextValidityProof", CiphertextValidityProof);
    register_opaque_json!(registry, "RangeProof", RangeProofWrapper);
}

impl Serializer for OpaqueWrapper {
    fn write(&self, writer: &mut Writer) {
        self.inner().serialize(writer.as_mut_bytes());
    }

    fn read(reader: &mut Reader) -> Result<Self, ReaderError> {
        Ok(match reader.read_u8()? {
            HASH_OPAQUE_ID => OpaqueWrapper::new(Hash::read(reader)?),
            ADDRESS_OPAQUE_ID => OpaqueWrapper::new(Address::read(reader)?),
            SIGNATURE_OPAQUE_ID => OpaqueWrapper::new(Signature::read(reader)?),
            CIPHERTEXT_OPAQUE_ID => OpaqueWrapper::new(CiphertextCache::read(reader)?),
            CIPHERTEXT_VALIDITY_PROOF_OPAQUE_ID => OpaqueWrapper::new(CiphertextValidityProof::read(reader)?),
            RANGE_PROOF_OPAQUE_ID => OpaqueWrapper::new(RangeProofWrapper(RangeProof::read(reader)?)),
            _ => return Err(ReaderError::InvalidValue)
        })
    }
}

#[cfg(test)]
mod tests {
    use crate::crypto::KeyPair;

    use super::*;
    use serde_json::json;
    use terminos_vm::OpaqueWrapper;

    #[test]
    fn test_address_serde() {
        register_opaque_types();
        
        let address = KeyPair::new().get_public_key().to_address(true);
        let opaque = OpaqueWrapper::new(address.clone());
        let v = json!(opaque);

        let opaque: OpaqueWrapper = serde_json::from_value(v)
            .unwrap();
        let address2: Address = opaque.into_inner()
            .expect("Failed to unwrap");

        assert_eq!(address, address2);
    }

    #[test]
    fn test_hash_serde() {
        register_opaque_types();
        
        let hash = Hash::max();
        let opaque = OpaqueWrapper::new(hash.clone());
        let v = json!(opaque);

        let opaque: OpaqueWrapper = serde_json::from_value(v)
            .unwrap();
        let hash2: Hash = opaque.into_inner()
            .expect("Failed to unwrap");

        assert_eq!(hash, hash2);
    }
}