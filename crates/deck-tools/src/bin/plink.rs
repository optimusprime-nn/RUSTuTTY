//! Plink: a session on the console, for scripts and tunnels.
//!
//!   plink [options] [user@]host [command]

use deck_core::settings::Value;
use deck_tools::{run_console, Args};
use std::io::Write;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = Args::parse(std::env::args().skip(1))?;
    let mut rest = args.rest.iter();
    let host = if args.load.is_some() { None } else { rest.next() };
    let mut target = args.target(host.map(String::as_str))?;
    let command: Vec<&str> = rest.map(String::as_str).collect();
    if !command.is_empty() {
        target.settings.set("RemoteCommand", Value::Str(command.join(" ")));
        // like ssh, a command has no terminal unless -t asks for one
        target.settings.set("NoPTY", Value::Int((args.no_pty != Some(false)) as u32));
    }
    let (why, code) = run_console(target).await?;
    // standard error may already be closed, which mustn't change the exit code
    let _ = writeln!(std::io::stderr(), "{why}");
    // the remote command's exit code, as PuTTY's Plink gives
    std::process::exit(code)
}
