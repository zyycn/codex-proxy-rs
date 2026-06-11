use thiserror::Error;

#[derive(Debug, Error)]
pub enum AuthError {
    #[error("password hash error: {0}")]
    PasswordHash(String),
    #[error("invalid api key encoding: {0}")]
    ApiKeyEncoding(#[from] base64::DecodeError),
    #[error("invalid api key pepper length")]
    InvalidPepperLength,
}

pub type AuthResult<T> = Result<T, AuthError>;

impl From<argon2::password_hash::Error> for AuthError {
    fn from(value: argon2::password_hash::Error) -> Self {
        Self::PasswordHash(value.to_string())
    }
}
