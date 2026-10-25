//! `mkinitramfs build <manifest> -o <out.cpio> [--test-fixtures]`
//! `mkinitramfs list <archive>`

use mkinitramfs::cpio::{self, S_IFDIR, S_IFMT};
use std::path::PathBuf;
use std::process::ExitCode;

const USAGE: &str = "usage:\n  mkinitramfs build <manifest> -o <out.cpio> [--test-fixtures]\n  mkinitramfs list <archive>";

fn main() -> ExitCode {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    let result = match arguments.first().map(String::as_str) {
        Some("build") => build(&arguments[1..]),
        Some("list") if arguments.len() == 2 => list(&arguments[1]),
        _ => Err(USAGE.to_string()),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("mkinitramfs: {error}");
            ExitCode::FAILURE
        }
    }
}

fn build(arguments: &[String]) -> Result<(), String> {
    let (mut manifest, mut output, mut fixtures) = (None, None, false);
    let mut arguments = arguments.iter();
    while let Some(argument) = arguments.next() {
        match argument.as_str() {
            "-o" => output = Some(PathBuf::from(arguments.next().ok_or(USAGE)?)),
            "--test-fixtures" => fixtures = true,
            path if manifest.is_none() && !path.starts_with('-') => {
                manifest = Some(PathBuf::from(path));
            }
            _ => return Err(USAGE.to_string()),
        }
    }
    let (manifest, output) = manifest.zip(output).ok_or(USAGE)?;
    let flavour = if fixtures {
        mkinitramfs::manifest::Flavour::Test
    } else {
        mkinitramfs::manifest::Flavour::Normal
    };
    let mut image = mkinitramfs::manifest::load(&manifest, flavour)?;
    if fixtures {
        image.add_test_fixtures()?;
    }
    let archive = image.to_cpio();
    // Write then rename, so a failed build never leaves a partial archive.
    let temporary = output.with_extension("cpio.tmp");
    std::fs::write(&temporary, &archive)
        .and_then(|()| std::fs::rename(&temporary, &output))
        .map_err(|error| format!("{}: {error}", output.display()))
}

fn list(path: &str) -> Result<(), String> {
    let archive = std::fs::read(path).map_err(|error| format!("{path}: {error}"))?;
    for member in cpio::read(&archive)? {
        let suffix = if member.mode & S_IFMT == S_IFDIR {
            "/"
        } else {
            ""
        };
        println!(
            "{:06o} {:>8} /{}{}",
            member.mode,
            member.data.len(),
            member.path,
            suffix
        );
    }
    Ok(())
}
