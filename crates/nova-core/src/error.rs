use thiserror::Error;

pub type Result<T> = std::result::Result<T, NovaError>;

#[derive(Debug, Error)]
pub enum NovaError {
    #[error("vault already exists: {0}")]
    VaultAlreadyExists(String),
    #[error("vault does not exist: {0}")]
    VaultNotFound(String),
    #[error("vault key is invalid or does not match this vault")]
    InvalidVaultKey,
    #[error("invalid key material: {0}")]
    InvalidKey(String),
    #[error("invalid request: {0}")]
    Validation(String),
    #[error("record not found: {kind} {id}")]
    NotFound { kind: &'static str, id: String },
    #[error("invalid state transition: {0}")]
    InvalidState(String),
    #[error("policy denied request: {0}")]
    PolicyDenied(String),
    #[error("idempotency key {request_id} was reused with different input")]
    IdempotencyConflict { request_id: String },
    #[error("idempotent result for request {request_id} is no longer replayable")]
    IdempotencyReplayUnavailable { request_id: String },
    #[error("encrypted record cannot be decrypted: {0}")]
    Crypto(String),
    #[error("audit chain verification failed at sequence {sequence}: {reason}")]
    AuditIntegrity { sequence: i64, reason: String },
    #[error("database error: {0}")]
    Database(#[from] rusqlite::Error),
    #[error("serialization error: {0}")]
    Serialization(#[from] serde_json::Error),
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("time error: {0}")]
    Time(String),
}

impl NovaError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::VaultAlreadyExists(_) => "NOVA_VAULT_ALREADY_EXISTS",
            Self::VaultNotFound(_) => "NOVA_VAULT_NOT_FOUND",
            Self::InvalidVaultKey => "NOVA_INVALID_VAULT_KEY",
            Self::InvalidKey(_) => "NOVA_INVALID_KEY",
            Self::Validation(_) => "NOVA_VALIDATION_FAILED",
            Self::NotFound { .. } => "NOVA_RECORD_NOT_FOUND",
            Self::InvalidState(_) => "NOVA_INVALID_STATE",
            Self::PolicyDenied(_) => "NOVA_POLICY_DENIED",
            Self::IdempotencyConflict { .. } => "NOVA_IDEMPOTENCY_CONFLICT",
            Self::IdempotencyReplayUnavailable { .. } => "NOVA_IDEMPOTENCY_REPLAY_UNAVAILABLE",
            Self::Crypto(_) => "NOVA_CRYPTO_FAILED",
            Self::AuditIntegrity { .. } => "NOVA_AUDIT_INTEGRITY_FAILED",
            Self::Database(_) => "NOVA_DATABASE_FAILED",
            Self::Serialization(_) => "NOVA_SERIALIZATION_FAILED",
            Self::Io(_) => "NOVA_IO_FAILED",
            Self::Time(_) => "NOVA_TIME_FAILED",
        }
    }

    pub fn retryable(&self) -> bool {
        matches!(self, Self::Database(_) | Self::Io(_))
    }
}
