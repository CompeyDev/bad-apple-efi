//! This crate is an example of the [`cargo-xtask`](https://github.com/matklad/cargo-xtask) pattern.

use std::env;
use std::fmt::Display;
use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;

const CARGO_MANIFEST_DIR: &str = env!("CARGO_MANIFEST_DIR");
const CRATE_NAME: &str = "bad-apple";
const PRINT_PREFIX: &str = "[xtask]";
const QEMU: &str = "qemu-system-x86_64";
#[cfg(target_os = "macos")]
const MACOS_FIRMWARE: &str = "../share/qemu/edk2-x86_64-code.fd"; // relative to dir with the binary

const DIST_DIR: &str = "dist";
const ESP_BOOT_PATH: [&str; 3] = ["EFI", "BOOT", "BOOTX64.EFI"];
const MIB: u64 = 1024 * 1024;

// TODO(nick): auto-generate these without introducing another crate.
const HELP_TEXT: &str = "\
Usage: cargo xtask <task> [cargo args...] [-- qemu args...]

Available tasks:
  build
    build for the UEFI target
  clean
    clean the workspace of produced artifacts
  dist
    build a release as a bootable disk image and a zip of its EFI partition
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
        Some("dist") => runner.task_dist(cargo_args),
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

    pub fn task_dist(&self, cargo_args: &[String]) -> Result<()> {
        self.task_build_release(cargo_args)?;

        let efi = fs::read(self.artifact_path(BuildProfile::Release))?;
        let dist = self.root.join(DIST_DIR);
        fs::create_dir_all(&dist)?;

        let zip = dist.join(format!("{CRATE_NAME}.zip"));
        write_zip(&zip, &efi)?;
        self.stdout(format!("wrote {}", zip.display()));

        let image = dist.join(format!("{CRATE_NAME}.img"));
        write_image(&image, &efi)?;
        self.stdout(format!("wrote {}", image.display()));

        Ok(())
    }

    pub fn task_clean(&self, cargo_args: &[String]) -> Result<()> {
        fs::remove_dir_all(".qemu")?;
        self.cargo("clean", cargo_args)?;
        Ok(())
    }

    pub fn task_qemu_run(&self, cargo_args: &[String], qemu_args: &[String]) -> Result<()> {
        if cfg!(not(any(target_os = "macos", target_os = "linux"))) {
            panic!("running QEMU is unsupported on this host");
        }

        // The guest behaves specially under QEMU, e.g. picking the smallest display mode, and
        // panics exit the emulator instead of hanging
        let mut build_args = vec![String::from("--features"), String::from("qemu")];
        build_args.extend(cargo_args.iter().cloned());
        self.task_build(&build_args)?;

        let qemu_dir = PathBuf::from(".qemu/efi/boot");
        fs::create_dir_all(&qemu_dir)?;
        fs::copy(self.artifact_path(BuildProfile::Debug), qemu_dir.join("bootx64.efi"))?;

        self.stdout("running project in QEMU");

        let mut qemu = Command::new(QEMU);
        qemu.arg("-nodefaults") // flag
            .args(["-cpu", "max,+invtsc"]) // QEMU hides the host's invariant TSC by default
            .args(["-machine", "q35"])
            .args(["-machine", "pcspk-audiodev=speaker"])
            .args(["-device", "intel-hda"])
            .args(["-device", "hda-output,audiodev=hda0"])
            .args(["-device", "isa-debug-exit,iobase=0xf4,iosize=0x04"]) // exit on guest panic
            .args(["-vga", "std"])
            .args(["-m", "512M"])
            .args(["-drive", "format=raw,file=fat:rw:.qemu"])
            .args(["-serial", "stdio"])
            .args(["-monitor", "vc"]);

        #[cfg(target_os = "macos")]
        {
            // Homebrew's QEMU only ships the firmware's code, which can't be loaded as BIOS
            let path = env::var_os("PATH").unwrap_or_default();
            let bin = env::split_paths(&path).find(|dir| dir.join(QEMU).is_file());
            let firmware = bin.ok_or("could not find QEMU")?.join(MACOS_FIRMWARE);
            let drive = format!("if=pflash,format=raw,readonly=on,file={}", firmware.display());

            qemu.args(["-drive", &drive])
                .args(["-machine", "q35,accel=hvf:tcg"]) // x86 guests are only accelerated on Intel Macs
                .args(["-audiodev", "coreaudio,id=speaker"])
                .args(["-audiodev", "coreaudio,id=hda0"])
                .args(["-display", "cocoa"]);
        }

        #[cfg(target_os = "linux")]
        {
            qemu.args(["-bios", "/usr/share/ovmf/x64/OVMF.4m.fd"])
                .arg("-enable-kvm") // flag
                .args(["-machine", "q35,accel=kvm:tcg"])
                .args(["-audiodev", "pipewire,id=speaker"])
                .args(["-audiodev", "pipewire,id=hda0"])
                .args(["-display", "gtk,gl=on"]); // software scaling makes audio stutter
        }

        let ok = qemu.args(qemu_args).status()?.success();

        if !ok { Err("QEMU command failed".into()) } else { Ok(()) }
    }
}

/// Writes a ZIP file containing the EFI System Partition, which can be extracted onto a bootable
/// FAT32 partition.
fn write_zip(path: &Path, efi: &[u8]) -> Result<()> {
    use zip::ZipWriter;
    use zip::write::SimpleFileOptions;

    let mut zip = ZipWriter::new(File::create(path)?);
    zip.start_file(ESP_BOOT_PATH.join("/"), SimpleFileOptions::default())?;
    zip.write_all(efi)?;
    zip.finish()?;
    Ok(())
}

/// Writes a disk image containing a GPT partition table along with a FAT32 EFI System Partition
/// with our boot payload.
fn write_image(path: &Path, efi: &[u8]) -> Result<()> {
    use fatfs::{FatType, FileSystem, FormatVolumeOptions, FsOptions};
    use fscommon::StreamSlice;
    use gpt::disk::LogicalBlockSize;
    use gpt::mbr::ProtectiveMBR;
    use gpt::{GptConfig, partition_types};

    // FAT32 needs at least 65525 clusters, so leave additional space
    let partition = (efi.len() as u64 + 4 * MIB).next_multiple_of(MIB).max(64 * MIB);
    let image_size = partition + 2 * MIB; // the partition table and its backup, both aligned

    let mut file = File::options().read(true).write(true).create(true).truncate(true).open(path)?;
    file.set_len(image_size)?;

    // Create a protective MBR entry, so tools which don't read GPT see a full partition of an unknown
    // type, rather than an empty disk
    let sectors = u32::try_from(image_size / 512 - 1).unwrap_or(u32::MAX);
    ProtectiveMBR::with_lb_size(sectors).overwrite_lba0(&mut file)?;

    let mut gpt = GptConfig::new()
        .writable(true)
        .logical_block_size(LogicalBlockSize::Lb512)
        .create_from_device(file, None)?;

    let id = gpt.add_partition(
        "EFI System",
        partition,
        partition_types::EFI,
        0,
        Some(MIB / 512), /* in sectors */
    )?;

    let start = gpt.partitions()[&id].bytes_start(LogicalBlockSize::Lb512)?;
    let file = gpt.write()?;

    // Format the partition
    let mut volume = StreamSlice::new(file, start, start + partition)?;
    let options = FormatVolumeOptions::new().fat_type(FatType::Fat32).volume_label(*b"BAD APPLE  ");
    fatfs::format_volume(&mut volume, options)?;

    // Copy the payload into the files ystem
    let fs = FileSystem::new(volume, FsOptions::new())?;
    {
        let mut dirs = ESP_BOOT_PATH[..2].iter();
        let dir = dirs.try_fold(fs.root_dir(), |dir, name| dir.create_dir(name))?;
        dir.create_file(ESP_BOOT_PATH[2])?.write_all(efi)?;
    }

    // Flush it
    fs.unmount()?;
    Ok(())
}
