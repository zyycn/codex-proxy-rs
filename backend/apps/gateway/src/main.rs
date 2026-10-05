//! 网关命令行入口，选择启动模式并创建异步运行时

use std::{
    env,
    error::Error,
    io::{self, Write},
    process::ExitCode,
};

use provider_openai::ensure_rustls_provider;

fn main() -> ExitCode {
    match run() {
        Ok(code) => code,
        Err(error) => {
            eprintln!("{error}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<ExitCode, Box<dyn Error + Send + Sync>> {
    ensure_rustls_provider();
    let mut arguments = env::args_os().skip(1);
    let command = arguments
        .next()
        .map(|value| {
            value
                .into_string()
                .map_err(|_| invalid_cli("command must be UTF-8"))
        })
        .transpose()?;
    match command.as_deref() {
        None | Some("serve") => {
            reject_extra_arguments(arguments)?;
            runtime()?.block_on(codex_proxy_rs::bootstrap::run())?;
            Ok(ExitCode::SUCCESS)
        }
        Some("help" | "--help" | "-h") => {
            reject_extra_arguments(arguments)?;
            println!(
                "Usage: codex-proxy-rs [serve]\n       codex-proxy-rs plugin <instance-id> <command> [options]\n\n  --help     Show help without loading configuration\n  --version  Show version\n  plugin --help  List enabled plugin commands without starting HTTP"
            );
            Ok(ExitCode::SUCCESS)
        }
        Some("--version" | "-V") => {
            reject_extra_arguments(arguments)?;
            println!(
                "{}",
                option_env!("CPR_VERSION").unwrap_or(env!("CARGO_PKG_VERSION"))
            );
            Ok(ExitCode::SUCCESS)
        }
        Some("plugin") => {
            let arguments = arguments
                .map(|value| {
                    value
                        .into_string()
                        .map_err(|_| invalid_cli("plugin arguments must be UTF-8"))
                })
                .collect::<Result<Vec<_>, _>>()?;
            let mut arguments = arguments.into_iter();
            let instance_id = arguments
                .next()
                .filter(|value| !matches!(value.as_str(), "--help" | "-h"));
            let name = arguments
                .next()
                .filter(|value| !matches!(value.as_str(), "--help" | "-h"));
            if instance_id.is_none() && name.is_some() {
                return Err(invalid_cli("unexpected plugin help arguments").into());
            }
            let output = runtime()?.block_on(codex_proxy_rs::bootstrap::plugin_command(
                codex_proxy_rs::bootstrap::PluginCommand {
                    instance_id,
                    name,
                    arguments: arguments.collect(),
                },
            ))?;
            io::stdout().lock().write_all(output.stdout.as_bytes())?;
            io::stderr().lock().write_all(output.stderr.as_bytes())?;
            Ok(ExitCode::from(output.exit_code))
        }
        Some(_) => Err(invalid_cli("unknown command; expected serve, plugin, or --help").into()),
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
