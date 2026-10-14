use std::ffi::OsString;
use std::path::Path;
use std::process::ExitCode;

use anyhow::{Result, bail};
use gloob::{cache, config::Config, ui};

const HELP: &str = "gloob

usage: gloob [--config <path>]
       gloob copy <path>

search gifs, copy the selected gif, and exit.
copy can also publish a local gif without opening the picker.
";

fn run(mut args: impl Iterator<Item = OsString>) -> Result<()> {
    match (args.next(), args.next(), args.next(), args.next()) {
        (None, None, None, None) => ui::run(Config::load(None)?),
        (Some(flag), Some(path), None, None) if flag == "--config" => {
            ui::run(Config::load(Some(Path::new(&path)))?)
        }
        (Some(command), None, None, None) if command == "--help" || command == "-h" => {
            print!("{HELP}");
            Ok(())
        }
        (Some(command), Some(path), None, None) if command == "copy" => {
            if let Some(warning) = cache::copy_path(Path::new(&path))? {
                eprintln!("gloob: {warning}");
            }
            println!("copied gif");
            Ok(())
        }
        _ => bail!("usage: gloob [--config <path>] | gloob copy <path>"),
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
