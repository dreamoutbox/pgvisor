use crate::error::CliError;
use crate::setup::generated_secrets::GeneratedSecrets;

/// Generates cryptographically strong random secrets using OS CSPRNG (`getrandom`).
pub struct SecretGenerator;

impl SecretGenerator {
    /// Generates a hex-encoded random string of `byte_len * 2` characters.
    pub fn random_hex(byte_len: usize) -> Result<String, CliError> {
        let mut buf = vec![0u8; byte_len];
        getrandom::fill(&mut buf).map_err(|err| {
            CliError::InvalidOption(format!("Failed to generate random secret: {err}"))
        })?;
        Ok(hex::encode(buf))
    }

    /// Generates all required secrets for a new PgVisor cluster.
    pub fn generate_all() -> Result<GeneratedSecrets, CliError> {
        Ok(GeneratedSecrets {
            postgres_password: Self::random_hex(16)?,
            cluster_secret: Self::random_hex(32)?,
            api_session_secret: Self::random_hex(32)?,
            s3_access_key: format!("pgvisor-{}", Self::random_hex(8)?),
            s3_secret_key: Self::random_hex(24)?,
        })
    }
}
