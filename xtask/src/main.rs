//! This crate is an example of the [`cargo-xtask`](https://github.com/matklad/cargo-xtask) pattern.

use std::env;
use std::fmt::Display;
use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::process::Command;

const CARGO_MANIFEST_DIR: &str = env!("CARGO_MANIFEST_DIR");
const CRATE_NAME: &str = "bad-apple";
const PRINT_PREFIX: &str = "[xtask]";

// TODO(nick): auto-generate these without introducing another crate.
const HELP_TEXT: &str = "\
Usage: cargo xtask <task> [cargo args...] [-- qemu args...]

Available tasks:
  build
    build for the UEFI target
  clean
    clean the workspace of produced artifacts
  qemu-run
    build and run the project using QEMU";

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

fn main() -> Result<()> {
    let mut args = env::args().skip(1);
    let maybe_argument = args.next().map(|argument| argument.trim().to_lowercase());

    // Split the remaining arguments at `--` into ones for cargo and ones for QEMU
    let rest: Vec<String> = args.collect();
    let (cargo_args, qemu_args) = match rest.iter().position(|arg| arg == "--") {
        Some(i) => (&rest[..i], &rest[i + 1..]),
        None => (&rest[..], &[][..]),
    };

    let runner = TaskRunner::new()?;

    match maybe_argument.as_deref() {
        Some("build") => runner.task_build_release(cargo_args),
        Some("clean") => runner.task_clean(cargo_args),
        Some("qemu-run") => runner.task_qemu_run(cargo_args, qemu_args),
        Some(invalid_task) => {
            eprintln!("{HELP_TEXT}");
            Err(format!("invalid task: {invalid_task}").into())
        }
        None => {
            println!("{HELP_TEXT}");
            Ok(())
        }
    }
}

struct TaskRunner {
    root: PathBuf,
}

enum BuildProfile {
    Debug,
    Release,
}

impl Display for BuildProfile {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BuildProfile::Debug => write!(f, "debug"),
            BuildProfile::Release => write!(f, "release"),
        }
    }
}

impl TaskRunner {
    pub fn new() -> Result<Self> {
        let root = match Path::new(CARGO_MANIFEST_DIR).ancestors().nth(1) {
            Some(found_root) => found_root.to_path_buf(),
            None => return Err("could not determine repo root".into()),
        };
        Ok(Self { root })
    }

    fn cargo(&self, command: &str, extra: &[String]) -> Result<()> {
        let args: Vec<&str> = command.split(' ').chain(extra.iter().map(String::as_str)).collect();
        self.stdout(format!("running: cargo {}", args.join(" ")));

        let mut cmd = Command::new("cargo");
        match cmd.current_dir(&self.root).args(&args).status()?.success() {
            true => Ok(()),
            false => Err("cargo command failed".into()),
        }
    }

    fn artifact_path(&self, profile: BuildProfile) -> PathBuf {
        self.root
            .join("target")
            .join("x86_64-unknown-uefi")
            .join(profile.to_string())
            .join(format!("{CRATE_NAME}.efi"))
    }

    #[allow(unused)]
    fn release_size(&self) -> Result<u64> {
        Ok(File::open(self.artifact_path(BuildProfile::Release))?.metadata()?.len())
    }

    fn stdout(&self, contents: impl AsRef<str>) {
        let contents = contents.as_ref();
        println!("{PRINT_PREFIX} {contents}");
    }

    #[allow(dead_code)]
    fn stderr(&self, contents: impl AsRef<str>) {
        let contents = contents.as_ref();
        eprintln!("{PRINT_PREFIX} {contents}");
    }

    pub fn task_build(&self, cargo_args: &[String]) -> Result<()> {
        self.cargo("build", cargo_args)?;
        Ok(())
    }

    pub fn task_build_release(&self, cargo_args: &[String]) -> Result<()> {
        self.cargo("build --release", cargo_args)?;
        self.stdout(format!("binary size: {}", self.release_size()?));
        Ok(())
    }

    pub fn task_clean(&self, cargo_args: &[String]) -> Result<()> {
        fs::remove_dir_all(".qemu")?;
        fs::remove_file("bin/bad_apple.mp4")?;
        fs::remove_file("bin/bad_apple.mid")?;
        self.cargo("clean", cargo_args)?;
        Ok(())
    }

    pub fn task_qemu_run(&self, cargo_args: &[String], qemu_args: &[String]) -> Result<()> {
        self.task_build(cargo_args)?;

        let qemu_dir = PathBuf::from(".qemu/efi/boot");
        fs::create_dir_all(&qemu_dir)?;
        fs::copy(self.artifact_path(BuildProfile::Debug), qemu_dir.join("bootx64.efi"))?;

        self.stdout("running project in QEMU");

        let ok = Command::new("qemu-system-x86_64")
            .arg("-nodefaults") // flag
            .args(["-bios", "/usr/share/ovmf/x64/OVMF.4m.fd"])
            .args(["-cpu", "max,+invtsc"]) // QEMU hides the host's invariant TSC by default
            .args(["-machine", "q35"])
            .arg("-enable-kvm") // flag
            .args(["-audiodev", "pipewire,id=speaker"])
            .args(["-audiodev", "pipewire,id=hda0"])
            .args(["-machine", "pcspk-audiodev=speaker"])
            .args(["-device", "intel-hda"])
            .args(["-device", "hda-output,audiodev=hda0"])
            .args(["-vga", "std"])
            .args(["-machine", "q35,accel=kvm:tcg"])
            .args(["-m", "512M"])
            .args(["-drive", "format=raw,file=fat:rw:.qemu"])
            .args(["-serial", "stdio"])
            .args(["-display", "gtk,gl=on"]) // software scaling makes audio stutter
            .args(["-monitor", "vc"])
            .args(qemu_args)
            .status()?
            .success();

        if !ok { Err("QEMU command failed".into()) } else { Ok(()) }
    }
}
