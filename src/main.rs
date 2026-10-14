use std::ffi::OsString;
use std::path::Path;
use std::process::ExitCode;

use anyhow::{Result, bail};
use gloob::{clipboard, media};

const HELP: &str = "gloob

usage: gloob copy <path>

copy a local gif to the wayland clipboard and exit.
";

fn run(mut args: impl Iterator<Item = OsString>) -> Result<()> {
    match (args.next(), args.next(), args.next()) {
        (Some(command), None, None) if command == "--help" || command == "-h" => {
            print!("{HELP}");
            Ok(())
        }
        (Some(command), Some(path), None) if command == "copy" => {
            let asset = media::cache_gif(Path::new(&path), &media::cache_dir()?)?;
            clipboard::publish(&asset)?;
            println!("copied gif");
            Ok(())
        }
        _ => bail!("usage: gloob copy <path>"),
    }
}

fn main() -> ExitCode {
    match run(std::env::args_os().skip(1)) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("gloob: {error:#}");
            ExitCode::FAILURE
        }
    }
}
