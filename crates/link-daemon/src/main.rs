use std::{path::PathBuf, process::ExitCode};

use clap::Parser;
use link_core::{ErrorKind, logging};

#[derive(Debug, Parser)]
#[command(name = "linkd", version, about = "User stream daemon for linkctl")]
struct Arguments {
    /// Serial, stable ID, USB path, or physical video node.
    #[arg(short = 'd', long, env = "LINKCTL_DEVICE")]
    device: Option<String>,
    /// Override the Unix-domain socket path.
    #[arg(long, env = "LINKCTL_DAEMON_SOCKET")]
    socket: Option<PathBuf>,
    /// Per-request and media startup timeout.
    #[arg(long, default_value = "3s")]
    timeout: humantime::Duration,
}

fn main() -> ExitCode {
    let arguments = Arguments::parse();
    logging::init(link_core::config::LogLevel::Info, false);
    let socket = match arguments.socket.map_or_else(link_ipc::socket_path, Ok) {
        Ok(socket) => socket,
        Err(error) => return report(error),
    };
    match link_daemon::run(link_daemon::DaemonOptions {
        socket,
        device: arguments.device,
        request_timeout: arguments.timeout.into(),
    }) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => report(error),
    }
}

fn report(error: link_core::LinkError) -> ExitCode {
    eprintln!("linkd: {}: {}", error.kind().code(), error.message());
    if error.kind() == ErrorKind::DaemonUnavailable {
        for (key, value) in error.details() {
            eprintln!("  {key}: {value}");
        }
    }
    ExitCode::from(error.process_exit().code())
}
