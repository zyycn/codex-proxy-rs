use std::{env, error::Error, io};

use codex_proxy_rs::bootstrap::config::BootstrapConfig;

fn main() -> Result<(), Box<dyn Error + Send + Sync>> {
    let mut arguments = env::args_os().skip(1);
    match arguments.next().as_deref().and_then(|value| value.to_str()) {
        None | Some("serve") => {
            reject_extra_arguments(arguments)?;
            let config = BootstrapConfig::load()?;
            runtime()?.block_on(codex_proxy_rs::bootstrap::services::run(config))
        }
        Some("rebuild-buckets") => {
            reject_extra_arguments(arguments)?;
            let config = BootstrapConfig::load()?;
            runtime()?.block_on(async move {
                let pool = codex_proxy_rs::infra::database::connect(config.database_url()).await?;
                let report = codex_proxy_rs::telemetry::rebuild::rebuild_buckets(&pool).await?;
                println!(
                    "request_time_buckets rebuilt: cutoff={}, deleted={}, rebuilt={}",
                    report.cutoff.to_rfc3339(),
                    report.deleted_rows,
                    report.rebuilt_rows
                );
                Ok(())
            })
        }
        Some(command) => Err(invalid_cli(&format!(
            "unknown command {command:?}; expected serve or rebuild-buckets"
        ))
        .into()),
    }
}

fn runtime() -> Result<tokio::runtime::Runtime, io::Error> {
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
}

fn reject_extra_arguments(mut arguments: impl Iterator) -> Result<(), io::Error> {
    if arguments.next().is_some() {
        return Err(invalid_cli("unexpected extra command arguments"));
    }
    Ok(())
}

fn invalid_cli(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}
