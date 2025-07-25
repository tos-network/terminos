use anyhow::Context as AnyhowContext;
use sha3::{Digest, Sha3_256};
use terminos_vm::{
    traits::Serializable,
    Context,
    EnvironmentError,
    FnInstance,
    FnParams,
    FnReturnType,
    OpaqueWrapper,
    Primitive,
    ValueCell,
    ValueError,
    U256
};
use crate::{
    contract::HASH_OPAQUE_ID,
    crypto::{hash, Hash, HASH_SIZE},
    serializer::{Serializer, Writer}
};

impl Serializable for Hash {
    fn serialize(&self, buffer: &mut Vec<u8>) -> usize {
        let mut writer = Writer::new(buffer);
        writer.write_u8(HASH_OPAQUE_ID);
        self.write(&mut writer);
        writer.total_write()
    }
}

pub fn hash_to_bytes_fn(zelf: FnInstance, _: FnParams, _: &mut Context) -> FnReturnType {
    let hash: &Hash = zelf?.as_opaque_type()?;
    let bytes = ValueCell::Bytes(hash.as_bytes().into());
    Ok(Some(bytes))
}

pub fn hash_to_array_fn(zelf: FnInstance, _: FnParams, _: &mut Context) -> FnReturnType {
    let hash: &Hash = zelf?.as_opaque_type()?;
    let bytes = hash.as_bytes()
        .into_iter()
        .map(|b| Primitive::U8(*b).into())
        .collect();

    Ok(Some(ValueCell::Object(bytes)))
}

pub fn hash_from_bytes_fn(_: FnInstance, mut params: FnParams, _: &mut Context) -> FnReturnType {
    let param = params.remove(0)
        .into_owned()?;
    let bytes = param.as_bytes()?;

    if bytes.len() != HASH_SIZE {
        return Err(EnvironmentError::InvalidParameter);
    }

    let hash = Hash::from_bytes(&bytes)
        .context("failed to create hash from bytes")?;

    Ok(Some(Primitive::Opaque(OpaqueWrapper::new(hash)).into()))
}

pub fn hash_from_array_fn(_: FnInstance, mut params: FnParams, _: &mut Context) -> FnReturnType {
    let param = params.remove(0)
        .into_owned()?;
    let values = param.as_vec()?;

    if values.len() != HASH_SIZE {
        return Err(EnvironmentError::InvalidParameter);
    }

    let mut bytes = Vec::with_capacity(HASH_SIZE);
    for value in values {
        let byte = value.as_u8()?;
        bytes.push(byte);
    }

    let hash = Hash::from_bytes(&bytes)
        .context("failed to create hash from bytes")?;

    Ok(Some(Primitive::Opaque(OpaqueWrapper::new(hash)).into()))
}

pub fn hash_from_u256_fn(_: FnInstance, mut params: FnParams, _: &mut Context) -> FnReturnType {
    let param = params.remove(0)
        .into_owned()?;

    let value = param.as_u256()?;
    let hash = Hash::from_bytes(&value.to_be_bytes())
        .context("failed to create hash from u256 be bytes")?;

    Ok(Some(Primitive::Opaque(OpaqueWrapper::new(hash)).into()))
}

pub fn hash_to_u256_fn(zelf: FnInstance, _: FnParams, _: &mut Context) -> FnReturnType {
    let hash: &Hash = zelf?.as_opaque_type()?;
    Ok(Some(Primitive::U256(U256::from_be_bytes(*hash.as_bytes())).into()))
}

pub fn hash_to_hex_fn(zelf: FnInstance, _: FnParams, _: &mut Context) -> FnReturnType {
    let hash: &Hash = zelf?.as_opaque_type()?;
    Ok(Some(Primitive::String(hex::encode(hash.as_bytes())).into()))
}

pub fn hash_from_hex_fn(_: FnInstance, mut params: FnParams, _: &mut Context) -> FnReturnType {
    let param = params.remove(0)
        .into_owned()?;
    let hex = param.as_string()?;

    if hex.len() != HASH_SIZE * 2 {
        return Err(EnvironmentError::InvalidParameter);
    }

    let hash = Hash::from_hex(hex)
        .context("failed to create hash from hex")?;

    Ok(Some(Primitive::Opaque(OpaqueWrapper::new(hash)).into()))
}

pub fn hash_zero_fn(_: FnInstance, _: FnParams, _: &mut Context) -> FnReturnType {
    let hash = Hash::zero();
    Ok(Some(Primitive::Opaque(OpaqueWrapper::new(hash)).into()))
}

pub fn hash_max_fn(_: FnInstance, _: FnParams, _: &mut Context) -> FnReturnType {
    let hash = Hash::max();
    Ok(Some(Primitive::Opaque(OpaqueWrapper::new(hash)).into()))
}

pub fn blake3_fn(_: FnInstance, mut params: FnParams, _: &mut Context) -> FnReturnType {
    let input = params.remove(0)
        .into_owned()?
        .as_vec()?
        .iter()
        .map(|v| v.as_u8())
        .collect::<Result<Vec<u8>, ValueError>>()?;

    let hash = hash(&input);
    Ok(Some(Primitive::Opaque(OpaqueWrapper::new(hash)).into()))
}

pub fn sha256_fn(_: FnInstance, mut params: FnParams, _: &mut Context) -> FnReturnType {
    let input = params.remove(0)
        .into_owned()?
        .as_vec()?
        .into_iter()
        .map(|v| v.as_u8())
        .collect::<Result<Vec<u8>, ValueError>>()?;

    let hash = Hash::new(Sha3_256::digest(&input).into());
    Ok(Some(Primitive::Opaque(OpaqueWrapper::new(hash)).into()))
}
