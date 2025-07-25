use thiserror::Error;
use terminos_common::rpc::InternalRpcError;

#[derive(Error, Debug, Clone, Copy)]
pub enum XSWDError {
    #[error("semaphore error")]
    SemaphoreError,
    #[error("Permission denied")]
    PermissionDenied,
    #[error("Permission invalid: method wasn't mentionned during handshake")]
    PermissionInvalid,
    #[error("Application not found")]
    ApplicationNotFound,
    #[error("Invalid application data")]
    InvalidApplicationData,
    #[error("Invalid application ID")]
    InvalidApplicationId,
    #[error("Application ID already used")]
    ApplicationIdAlreadyUsed,
    #[error("Invalid hexadecimal for application ID")]
    InvalidHexaApplicationId,
    #[error("Application name is too long")]
    ApplicationNameTooLong,
    #[error("Application description is too long")]
    ApplicationDescriptionTooLong,
    #[error("Invalid URL format")]
    InvalidURLFormat,
    #[error("Invalid origin")]
    InvalidOrigin,
    #[error("Too many permissions")]
    TooManyPermissions,
    #[error("Unknown method requested in permissions list")]
    UnknownMethodInPermissionsList,
    #[error("Application permissions are not signed")]
    ApplicationPermissionsNotSigned,
    #[error("Invalid signature for application data")]
    InvalidSignatureForApplicationData
}

impl From<XSWDError> for InternalRpcError {
    fn from(e: XSWDError) -> Self {
        let err = e.into();
        let id = e as i16;
        InternalRpcError::CustomAny(10 + id, err)
    }
}
