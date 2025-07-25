use anyhow::Context as AnyhowContext;
use terminos_vm::{
    impl_opaque,
    traits::Serializable,
    Context,
    EnvironmentError,
    FnInstance,
    FnParams,
    FnReturnType,
    Primitive,
    ValueError
};

use crate::{
    contract::SIGNATURE_OPAQUE_ID,
    crypto::{Address, Signature, SIGNATURE_SIZE},
    serializer::{Serializer, Writer}
};

impl_opaque!(
    "Signature",
    Signature
);
impl_opaque!(
    "Signature",
    Signature,
    json
);

impl Serializable for Signature {
    fn get_size(&self) -> usize {
        SIGNATURE_SIZE
    }

    fn is_serializable(&self) -> bool {
        true
    }

    fn serialize(&self, buffer: &mut Vec<u8>) -> usize {
        let mut writer = Writer::new(buffer);
        writer.write_u8(SIGNATURE_OPAQUE_ID);
        self.write(&mut writer);
        writer.total_write()
    }
}

pub fn signature_from_bytes_fn(_: FnInstance, mut params: FnParams, _: &mut Context) -> FnReturnType {
    let param = params.remove(0)
        .into_owned()?;
    let param = param.as_vec()?;

    if param.len() != SIGNATURE_SIZE {
        return Err(EnvironmentError::InvalidParameter);
    }

    let bytes = param.iter()
        .map(|v| v.as_u8())
        .collect::<Result<Vec<_>, ValueError>>()?;

    let signature = Signature::from_bytes(&bytes)
        .context("signature from bytes")?;
    Ok(Some(Primitive::Opaque(signature.into()).into()))
}

pub fn signature_verify_fn(zelf: FnInstance, mut params: FnParams, _: &mut Context) -> FnReturnType {
    let signature: &Signature = zelf?.as_opaque_type()?;

    let address: Address = params.remove(0)
        .into_owned()?
        .into_opaque_type()?;

    let param = params.remove(0)
        .into_owned()?;
    let data = param.as_vec()?
        .iter()
        .map(|v| v.as_u8())
        .collect::<Result<Vec<_>, ValueError>>()?;

    let key = address.to_public_key()
        .decompress()
        .context("decompress key for signature")?;
    Ok(Some(Primitive::Boolean(signature.verify(&data, &key)).into()))
}

#[cfg(test)]
mod tests {
    use curve25519_dalek::Scalar;
    use super::*;

    #[test]
    fn test_serde() {
        let signature = Signature::new(Scalar::from(1u64), Scalar::from(2u64));
        let v = serde_json::to_value(&signature).unwrap();

        let signature2: Signature = serde_json::from_value(v)
            .unwrap();

        assert_eq!(signature, signature2);
    }
}