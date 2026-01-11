//! This crate is an example of the [`cargo-xtask`](https://github.com/matklad/cargo-xtask) pattern.

use std::env;
use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::process::Command;

const CARGO_MANIFEST_DIR: &str = env!("CARGO_MANIFEST_DIR");
const CRATE_NAME: &str = "bad-apple";
const PRINT_PREFIX: &str = "[xtask]";

// TODO(nick): auto-generate these without introducing another crate.
const HELP_TEXT: &str = "\
Available tasks:
  build
    build for the UEFI target
  clean
    clean the workspace of produced artifacts
  qemu-run
    build and run the project using QEMU";

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

fn main() -> Result<()> {
    let maybe_argument = env::args().nth(1).as_ref().map(|argument| argument.trim().to_lowercase());

    let runner = TaskRunner::new()?;

    match maybe_argument.as_deref() {
        Some("build") => runner.task_build(),
        Some("clean") => runner.task_clean(),
        Some("qemu-run") => runner.task_qemu_run(),
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

impl TaskRunner {
    pub fn new() -> Result<Self> {
        let root = match Path::new(CARGO_MANIFEST_DIR).ancestors().nth(1) {
            Some(found_root) => found_root.to_path_buf(),
            None => return Err("could not determine repo root".into()),
        };
        Ok(Self { root })
    }

    fn cargo(&self, args: &'static str) -> Result<()> {
        self.stdout(format!("running: cargo {args}"));

        let mut cmd = Command::new("cargo");
        match cmd.current_dir(&self.root).args(args.trim().split(" ")).status()?.success() {
            true => Ok(()),
            false => Err("cargo command failed".into()),
        }
    }

    fn artifact_path(&self) -> PathBuf {
        self.root
            .join("target")
            .join("x86_64-unknown-uefi")
            .join("release")
            .join(format!("{CRATE_NAME}.efi"))
    }

    #[allow(unused)]
    fn release_size(&self) -> Result<u64> {
        Ok(File::open(self.artifact_path())?.metadata()?.len())
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

    pub fn task_build(&self) -> Result<()> {
        self.cargo("build")?;
        Ok(())
    }

    #[allow(unused)]
    pub fn task_build_release(&self) -> Result<()> {
        self.cargo("build --release")?;
        self.stdout(format!("binary size: {}", self.release_size()?));
        Ok(())
    }

    pub fn task_clean(&self) -> Result<()> {
        fs::remove_dir_all(".qemu")?;
        fs::remove_file("bin/bad_apple.mp4")?;
        fs::remove_file("bin/bad_apple.mid")?;
        self.cargo("clean")?;
        Ok(())
    }

    pub fn task_qemu_run(&self) -> Result<()> {
        self.task_build_release()?;

        let qemu_dir = PathBuf::from(".qemu/efi/boot");
        fs::create_dir_all(&qemu_dir)?;
        fs::copy(self.artifact_path(), qemu_dir.join("bootx64.efi"))?;

        self.stdout("running project in QEMU");

        let ok = Command::new("qemu-system-x86_64")
            .arg("-nodefaults") // flag
            .args(["-bios", "/usr/share/ovmf/x64/OVMF.4m.fd"])
            .args(["-cpu", "max"])
            .args(["-machine", "q35"])
            .arg("-enable-kvm") // flag
            .args(["-audiodev", "pa,id=speaker"])
            .args(["-machine", "pcspk-audiodev=speaker"])
            .args(["-vga", "std"])
            .args(["-machine", "q35,accel=kvm:tcg"])
            .args(["-m", "512M"])
            .args(["-drive", "format=raw,file=fat:rw:.qemu"])
            .args(["-serial", "stdio"])
            .args(["-display", "gtk"])
            .args(["-monitor", "vc:256x192"])
            .status()?
            .success();

        if !ok { Err("QEMU command failed".into()) } else { Ok(()) }
    }
}
