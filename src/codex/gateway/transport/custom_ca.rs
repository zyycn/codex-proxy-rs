use std::{env, fs, io, path::PathBuf, sync::Arc};

use rustls::{ClientConfig, RootCertStore};
use rustls_pki_types::{
    pem::{self, PemObject, SectionKind},
    CertificateDer,
};
use thiserror::Error;

pub const CODEX_CA_CERT_ENV: &str = "CODEX_CA_CERTIFICATE";
pub const SSL_CERT_FILE_ENV: &str = "SSL_CERT_FILE";

const CA_CERT_HINT: &str = "If you set CODEX_CA_CERTIFICATE or SSL_CERT_FILE, ensure it points to a PEM file containing one or more CERTIFICATE blocks, or unset it to use system roots.";

type PemSection = (SectionKind, Vec<u8>);

#[derive(Debug, Error)]
pub enum CustomCaError {
    #[error(
        "Failed to read CA certificate file {} selected by {}: {source}. {hint}",
        path.display(),
        source_env,
        hint = CA_CERT_HINT
    )]
    ReadCaFile {
        source_env: &'static str,
        path: PathBuf,
        source: io::Error,
    },
    #[error(
        "Failed to load CA certificates from {} selected by {}: {detail}. {hint}",
        path.display(),
        source_env,
        hint = CA_CERT_HINT
    )]
    InvalidCaFile {
        source_env: &'static str,
        path: PathBuf,
        detail: String,
    },
    #[error(
        "Failed to parse certificate #{certificate_index} from {} selected by {}: {source}. {hint}",
        path.display(),
        source_env,
        hint = CA_CERT_HINT
    )]
    RegisterCertificate {
        source_env: &'static str,
        path: PathBuf,
        certificate_index: usize,
        source: reqwest::Error,
    },
    #[error(
        "Failed to register certificate #{certificate_index} from {} selected by {} in rustls root store: {source}. {hint}",
        path.display(),
        source_env,
        hint = CA_CERT_HINT
    )]
    RegisterRustlsCertificate {
        source_env: &'static str,
        path: PathBuf,
        certificate_index: usize,
        source: rustls::Error,
    },
    #[error("Failed to build HTTP client while using CA bundle from {} ({}): {source}", source_env, path.display())]
    BuildClientWithCustomCa {
        source_env: &'static str,
        path: PathBuf,
        source: reqwest::Error,
    },
    #[error("Failed to build HTTP client while using system root certificates: {0}")]
    BuildClientWithSystemRoots(reqwest::Error),
    #[error("Failed to load native root certificates for custom CA transport: {0}")]
    LoadNativeRoots(io::Error),
}

impl From<CustomCaError> for io::Error {
    fn from(error: CustomCaError) -> Self {
        match error {
            CustomCaError::ReadCaFile { ref source, .. } => io::Error::new(source.kind(), error),
            CustomCaError::InvalidCaFile { .. }
            | CustomCaError::RegisterCertificate { .. }
            | CustomCaError::RegisterRustlsCertificate { .. } => {
                io::Error::new(io::ErrorKind::InvalidData, error)
            }
            CustomCaError::BuildClientWithCustomCa { .. }
            | CustomCaError::BuildClientWithSystemRoots(_)
            | CustomCaError::LoadNativeRoots(_) => io::Error::other(error),
        }
    }
}

pub fn build_reqwest_client_with_custom_ca(
    builder: reqwest::ClientBuilder,
) -> Result<reqwest::Client, CustomCaError> {
    build_reqwest_client_with_env(&ProcessEnv, builder)
}

pub fn custom_ca_env_cache_key() -> Option<String> {
    ProcessEnv
        .configured_ca_bundle()
        .map(|bundle| format!("{}={}", bundle.source_env, bundle.path.display()))
}

pub fn maybe_build_rustls_client_config_with_custom_ca(
) -> Result<Option<Arc<ClientConfig>>, CustomCaError> {
    maybe_build_rustls_client_config_with_env(&ProcessEnv)
}

fn build_reqwest_client_with_env(
    env_source: &dyn EnvSource,
    mut builder: reqwest::ClientBuilder,
) -> Result<reqwest::Client, CustomCaError> {
    let Some(bundle) = env_source.configured_ca_bundle() else {
        return builder
            .build()
            .map_err(CustomCaError::BuildClientWithSystemRoots);
    };

    builder = builder.use_rustls_tls();
    for (idx, cert) in bundle.load_certificates()?.iter().enumerate() {
        let certificate = reqwest::Certificate::from_der(cert.as_ref()).map_err(|source| {
            CustomCaError::RegisterCertificate {
                source_env: bundle.source_env,
                path: bundle.path.clone(),
                certificate_index: idx + 1,
                source,
            }
        })?;
        builder = builder.add_root_certificate(certificate);
    }
    builder
        .build()
        .map_err(|source| CustomCaError::BuildClientWithCustomCa {
            source_env: bundle.source_env,
            path: bundle.path,
            source,
        })
}

fn maybe_build_rustls_client_config_with_env(
    env_source: &dyn EnvSource,
) -> Result<Option<Arc<ClientConfig>>, CustomCaError> {
    let Some(bundle) = env_source.configured_ca_bundle() else {
        return Ok(None);
    };

    let mut root_store = native_root_store().map_err(CustomCaError::LoadNativeRoots)?;
    for (idx, cert) in bundle.load_certificates()?.into_iter().enumerate() {
        root_store
            .add(cert)
            .map_err(|source| CustomCaError::RegisterRustlsCertificate {
                source_env: bundle.source_env,
                path: bundle.path.clone(),
                certificate_index: idx + 1,
                source,
            })?;
    }

    Ok(Some(Arc::new(
        ClientConfig::builder()
            .with_root_certificates(root_store)
            .with_no_client_auth(),
    )))
}

pub fn native_root_store() -> Result<RootCertStore, io::Error> {
    let mut root_store = RootCertStore::empty();
    let rustls_native_certs::CertificateResult { certs, errors, .. } =
        rustls_native_certs::load_native_certs();
    if !errors.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("failed to load native root certificates: {errors:?}"),
        ));
    }

    let (added, _) = root_store.add_parsable_certificates(certs);
    if added == 0 {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            "no native root certificates found",
        ));
    }

    Ok(root_store)
}

trait EnvSource {
    fn var(&self, key: &str) -> Option<String>;

    fn non_empty_path(&self, key: &str) -> Option<PathBuf> {
        self.var(key)
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
    }

    fn configured_ca_bundle(&self) -> Option<ConfiguredCaBundle> {
        self.non_empty_path(CODEX_CA_CERT_ENV)
            .map(|path| ConfiguredCaBundle {
                source_env: CODEX_CA_CERT_ENV,
                path,
            })
            .or_else(|| {
                self.non_empty_path(SSL_CERT_FILE_ENV)
                    .map(|path| ConfiguredCaBundle {
                        source_env: SSL_CERT_FILE_ENV,
                        path,
                    })
            })
    }
}

struct ProcessEnv;

impl EnvSource for ProcessEnv {
    fn var(&self, key: &str) -> Option<String> {
        env::var(key).ok()
    }
}

struct ConfiguredCaBundle {
    source_env: &'static str,
    path: PathBuf,
}

impl ConfiguredCaBundle {
    fn load_certificates(&self) -> Result<Vec<CertificateDer<'static>>, CustomCaError> {
        let pem_data = fs::read(&self.path).map_err(|source| CustomCaError::ReadCaFile {
            source_env: self.source_env,
            path: self.path.clone(),
            source,
        })?;
        let normalized = normalize_trusted_certificate_labels(&pem_data);
        let mut certificates = Vec::new();
        for section in PemSection::pem_slice_iter(normalized.as_bytes()) {
            let (kind, der) = section.map_err(|error| self.pem_parse_error(&error))?;
            if kind == SectionKind::Certificate {
                certificates.push(CertificateDer::from(der));
            }
        }
        if certificates.is_empty() {
            return Err(self.pem_parse_error(&pem::Error::NoItemsFound));
        }
        Ok(certificates)
    }

    fn pem_parse_error(&self, error: &pem::Error) -> CustomCaError {
        let detail = match error {
            pem::Error::NoItemsFound => "no certificates found in PEM file".to_string(),
            _ => format!("failed to parse PEM file: {error}"),
        };
        CustomCaError::InvalidCaFile {
            source_env: self.source_env,
            path: self.path.clone(),
            detail,
        }
    }
}

fn normalize_trusted_certificate_labels(pem_data: &[u8]) -> String {
    String::from_utf8_lossy(pem_data)
        .replace("BEGIN TRUSTED CERTIFICATE", "BEGIN CERTIFICATE")
        .replace("END TRUSTED CERTIFICATE", "END CERTIFICATE")
}

#[cfg(test)]
mod tests {
    use std::{collections::HashMap, fs, path::PathBuf};

    use tempfile::TempDir;

    use super::*;

    const TEST_CERT: &str = include_str!("../../../../tests/fixtures/test-ca.pem");

    struct MapEnv {
        values: HashMap<String, String>,
    }

    impl EnvSource for MapEnv {
        fn var(&self, key: &str) -> Option<String> {
            self.values.get(key).cloned()
        }
    }

    fn map_env(pairs: &[(&str, &str)]) -> MapEnv {
        MapEnv {
            values: pairs
                .iter()
                .map(|(key, value)| ((*key).to_string(), (*value).to_string()))
                .collect(),
        }
    }

    fn write_cert_file(temp_dir: &TempDir, name: &str, contents: &str) -> PathBuf {
        let path = temp_dir.path().join(name);
        fs::write(&path, contents).unwrap();
        path
    }

    #[test]
    fn ca_path_prefers_codex_env() {
        let env = map_env(&[
            (CODEX_CA_CERT_ENV, "/tmp/codex.pem"),
            (SSL_CERT_FILE_ENV, "/tmp/fallback.pem"),
        ]);

        assert_eq!(
            env.configured_ca_bundle().map(|bundle| bundle.path),
            Some(PathBuf::from("/tmp/codex.pem"))
        );
    }

    #[test]
    fn ca_path_falls_back_to_ssl_cert_file() {
        let env = map_env(&[(SSL_CERT_FILE_ENV, "/tmp/fallback.pem")]);

        assert_eq!(
            env.configured_ca_bundle().map(|bundle| bundle.path),
            Some(PathBuf::from("/tmp/fallback.pem"))
        );
    }

    #[test]
    fn ca_path_ignores_empty_values() {
        let env = map_env(&[
            (CODEX_CA_CERT_ENV, ""),
            (SSL_CERT_FILE_ENV, "/tmp/fallback.pem"),
        ]);

        assert_eq!(
            env.configured_ca_bundle().map(|bundle| bundle.path),
            Some(PathBuf::from("/tmp/fallback.pem"))
        );
    }

    #[test]
    fn rustls_config_uses_custom_ca_bundle_when_configured() {
        let temp_dir = TempDir::new().unwrap();
        let cert_path = write_cert_file(&temp_dir, "ca.pem", TEST_CERT);
        let env = map_env(&[(CODEX_CA_CERT_ENV, cert_path.to_string_lossy().as_ref())]);

        let config = maybe_build_rustls_client_config_with_env(&env)
            .expect("rustls config")
            .expect("custom CA config should be present");

        assert!(config.enable_sni);
    }

    #[test]
    fn rustls_config_reports_invalid_ca_file() {
        let temp_dir = TempDir::new().unwrap();
        let cert_path = write_cert_file(&temp_dir, "empty.pem", "");
        let env = map_env(&[(CODEX_CA_CERT_ENV, cert_path.to_string_lossy().as_ref())]);

        let error = maybe_build_rustls_client_config_with_env(&env).unwrap_err();

        assert!(matches!(error, CustomCaError::InvalidCaFile { .. }));
    }
}
