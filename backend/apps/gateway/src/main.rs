use std::{env, error::Error, io};

fn main() -> Result<(), Box<dyn Error + Send + Sync>> {
    let mut arguments = env::args_os().skip(1);
    match arguments.next().as_deref().and_then(|value| value.to_str()) {
        None | Some("serve") => {
            reject_extra_arguments(arguments)?;
            runtime()?.block_on(codex_proxy_rs::bootstrap::run())?;
            Ok(())
        }
        Some(command) => {
            Err(invalid_cli(&format!("unknown command {command:?}; expected serve")).into())
        }
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
