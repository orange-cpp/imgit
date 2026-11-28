//! ImGit's build tasks, run as `cargo xtask <task>`.
//!
//! * `release`: builds the release executable (`cargo build --release -p imgit`) and packs it in place with
//!   UPX, which must be on `PATH`. Extra arguments go to `cargo build` (`cargo xtask release --locked`).

use std::env;
use std::ffi::OsString;
use std::fmt;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};

/// What `upx` is asked to do: the smallest output the Windows and Linux release packages use.
const UPX_PACK_ARGS: [&str; 2] = ["--best", "--lzma"];

fn main() -> ExitCode {
    match dispatch(env::args_os().skip(1), release) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("xtask: {error}");
            ExitCode::FAILURE
        }
    }
}

fn dispatch(
    mut args: impl Iterator<Item = OsString>,
    release: impl FnOnce(Vec<OsString>) -> Result<(), Error>,
) -> Result<(), Error> {
    match args.next().as_deref().and_then(|task| task.to_str()) {
        Some("release") => release(args.collect()),
        _ => Err(Error::Usage),
    }
}

#[derive(Debug)]
enum Error {
    Usage,
    /// A tool could not be started.
    Spawn {
        tool: &'static str,
        source: io::Error,
    },
    /// A tool ran and reported failure.
    Failed(&'static str),
    Io(io::Error),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Usage => write!(f, "usage: cargo xtask release [cargo build arguments]"),
            Self::Spawn {
                tool: "upx",
                source,
            } => write!(
                f,
                "could not run upx ({source}); install UPX (https://upx.github.io) and put it on PATH"
            ),
            Self::Spawn { tool, source } => write!(f, "could not run {tool}: {source}"),
            Self::Failed(tool) => write!(f, "{tool} failed"),
            Self::Io(source) => write!(f, "{source}"),
        }
    }
}

impl From<io::Error> for Error {
    fn from(source: io::Error) -> Self {
        Self::Io(source)
    }
}

/// Builds the release executable and packs it with UPX.
fn release(build_args: Vec<OsString>) -> Result<(), Error> {
    let cargo = env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let root = workspace_root();
    let executable = release_directory(&root).join(format!("imgit{}", env::consts::EXE_SUFFIX));
    build_and_pack(&root, &executable, cargo, build_args, |command| {
        command.status().map(|status| status.success())
    })
}

fn build_and_pack(
    root: &Path,
    executable: &Path,
    cargo: OsString,
    build_args: Vec<OsString>,
    mut succeeds: impl FnMut(&mut Command) -> io::Result<bool>,
) -> Result<(), Error> {
    run(
        "cargo",
        Command::new(cargo)
            .current_dir(root)
            .args(["build", "--release", "--package", "imgit"])
            .args(build_args),
        &mut succeeds,
    )?;

    let before = executable.metadata()?.len();
    // Cargo normally puts its own (unpacked) executable back on every build, so this is a safety net: UPX
    // refuses to pack a file twice.
    if is_packed(executable, &mut succeeds)? {
        println!(
            "{} is already packed ({})",
            executable.display(),
            megabytes(before)
        );
        return Ok(());
    }
    run(
        "upx",
        Command::new("upx").args(UPX_PACK_ARGS).arg(executable),
        &mut succeeds,
    )?;
    // The packed file must unpack to what went in.
    run(
        "upx",
        Command::new("upx").arg("-t").arg(executable),
        &mut succeeds,
    )?;
    let after = executable.metadata()?.len();
    println!(
        "{}: {} -> {}",
        executable.display(),
        megabytes(before),
        megabytes(after)
    );
    Ok(())
}

/// The workspace's folder: two above this crate's.
fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("crates/xtask lies two folders below the workspace root")
        .to_path_buf()
}

/// Where `cargo build --release` puts the executable: under `CARGO_TARGET_DIR` when set, else `target`.
fn release_directory(root: &Path) -> PathBuf {
    let target = env::var_os("CARGO_TARGET_DIR").map_or_else(|| root.join("target"), PathBuf::from);
    target.join("release")
}

/// Whether UPX has packed `executable` already (`upx -t` succeeds only on a packed file).
fn is_packed(
    executable: &Path,
    succeeds: &mut impl FnMut(&mut Command) -> io::Result<bool>,
) -> Result<bool, Error> {
    succeeds(
        Command::new("upx")
            .args(["-t", "-qq"])
            .arg(executable)
            .stdout(Stdio::null())
            .stderr(Stdio::null()),
    )
    .map_err(|source| Error::Spawn {
        tool: "upx",
        source,
    })
}

fn run(
    tool: &'static str,
    command: &mut Command,
    succeeds: &mut impl FnMut(&mut Command) -> io::Result<bool>,
) -> Result<(), Error> {
    if succeeds(command).map_err(|source| Error::Spawn { tool, source })? {
        Ok(())
    } else {
        Err(Error::Failed(tool))
    }
}

fn megabytes(bytes: u64) -> String {
    #[allow(clippy::cast_precision_loss)] // a file size, shown to two decimals
    let megabytes = bytes as f64 / (1024.0 * 1024.0);
    format!("{megabytes:.2} MB")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dispatch_passes_extra_build_arguments_to_release_unchanged() {
        dispatch(
            ["release", "--locked", "--features", "two words"]
                .map(OsString::from)
                .into_iter(),
            |args| {
                assert_eq!(args, ["--locked", "--features", "two words"]);
                Ok(())
            },
        )
        .unwrap();
        let error = dispatch(["unknown"].into_iter().map(OsString::from), |_| {
            panic!("unknown tasks cannot begin a release")
        })
        .unwrap_err();
        assert_eq!(
            error.to_string(),
            "usage: cargo xtask release [cargo build arguments]"
        );
    }

    #[test]
    fn release_builds_probes_packs_and_verifies_the_executable_in_order() {
        let root = workspace_root();
        // Only metadata is read by the release code. The command runner is mocked.
        let executable = env::current_exe().unwrap();
        let mut commands = Vec::new();
        build_and_pack(
            &root,
            &executable,
            "custom-cargo".into(),
            vec!["--locked".into()],
            |command| {
                let program = command.get_program().to_owned();
                let args: Vec<_> = command.get_args().map(|arg| arg.to_owned()).collect();
                let dir = command.get_current_dir().map(Path::to_owned);
                commands.push((program, args, dir));
                Ok(commands.len() != 2)
            },
        )
        .unwrap();

        assert_eq!(commands.len(), 4);
        assert_eq!(commands[0].0, "custom-cargo");
        assert_eq!(
            commands[0].1,
            ["build", "--release", "--package", "imgit", "--locked"]
        );
        assert_eq!(commands[0].2.as_deref(), Some(root.as_path()));
        for (index, flags) in [
            (1, &["-t", "-qq"][..]),
            (2, UPX_PACK_ARGS.as_slice()),
            (3, &["-t"][..]),
        ] {
            let (tool, args, _) = &commands[index];
            let (file, options) = args.split_last().unwrap();
            assert_eq!(tool, "upx");
            assert_eq!(options, flags);
            assert_eq!(Path::new(file), executable);
        }
    }

    #[test]
    fn release_skips_repacking_and_reports_command_or_file_failures() {
        let root = workspace_root();
        let executable = env::current_exe().unwrap();
        let mut count = 0;
        build_and_pack(&root, &executable, "cargo".into(), Vec::new(), |_| {
            count += 1;
            Ok(true)
        })
        .unwrap();
        assert_eq!(count, 2);

        let build = build_and_pack(&root, &executable, "cargo".into(), Vec::new(), |_| {
            Ok(false)
        })
        .unwrap_err();
        assert!(matches!(build, Error::Failed("cargo")));
        assert_eq!(build.to_string(), "cargo failed");

        let mut count = 0;
        let launch = build_and_pack(&root, &executable, "cargo".into(), Vec::new(), |_| {
            count += 1;
            if count == 1 {
                Ok(true)
            } else {
                Err(io::Error::new(io::ErrorKind::NotFound, "UPX missing"))
            }
        })
        .unwrap_err();
        assert!(matches!(launch, Error::Spawn { tool: "upx", .. }));
        assert!(launch.to_string().contains("install UPX"));

        let missing = executable.join("missing");
        let error =
            build_and_pack(&root, &missing, "cargo".into(), Vec::new(), |_| Ok(true)).unwrap_err();
        assert!(matches!(error, Error::Io(_)));
    }

    #[test]
    fn the_workspace_root_holds_the_workspace_manifest() {
        assert!(workspace_root().join("Cargo.toml").is_file());
        assert!(workspace_root().join("crates").join("xtask").is_dir());
    }

    #[test]
    fn sizes_read_in_megabytes() {
        assert_eq!(megabytes(8_619_008), "8.22 MB");
        assert_eq!(megabytes(0), "0.00 MB");
    }
}
