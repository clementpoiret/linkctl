//! `linkctl` executable entry point.

use std::process::ExitCode;

fn main() -> ExitCode {
    ExitCode::from(link_cli::run_from_env())
}
