use std::hash::{BuildHasher, BuildHasherDefault, DefaultHasher};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::{env, fs};

use xshell::{cmd, Shell};

const VIDEO_SOURCES: [&str; 2] = ["bin/bad_apple.webm", "bin/bad_apple.mp4"];
const VP9_ENCODE_ARGS: [&str; 10] =
    ["-c:v", "libvpx-vp9", "-crf", "37", "-b:v", "0", "-cpu-used", "2", "-row-mt", "1"];

const SUPPORTED_PIX_FMTS: [&str; 5] = ["yuv420p", "yuv422p", "yuv440p", "yuv444p", "gbrp"];

const FREESTANDING_DIR: &str = "vendor/freestanding";
const VPX_SRC_DIR: &str = "vendor/libvpx";
const VPX_DISABLED_FEATURES: [&str; 10] = [
    "vp8",
    "vp9-encoder",
    "postproc",
    "multithread",
    "unit-tests",
    "webm-io",
    "libyuv",
    "examples",
    "tools",
    "docs",
];

type Result<T = ()> = std::result::Result<T, Box<dyn std::error::Error>>;

fn main() -> Result {
    println!("cargo::rerun-if-changed={FREESTANDING_DIR}");
    println!("cargo::rerun-if-changed=bin");

    let out_dir = PathBuf::from(env::var("OUT_DIR")?);
    build_libvpx(&out_dir)?;
    convert_video(&out_dir)?;

    Ok(())
}

fn build_libvpx(out_dir: &Path) -> Result {
    // Include the freestanding dir as the first path, so it wins over the generated config
    // header file when possible
    let freestanding = PathBuf::from(env::var("CARGO_MANIFEST_DIR")?).join(FREESTANDING_DIR);
    let cflags = format!(
        "-target x86_64-windows-msvc -mavx2 -mfma -ffreestanding -I{}",
        freestanding.display()
    );

    // Old archives are never dropped even if the new config no longer needs them, we
    // do it manually
    let archive = out_dir.join("build/libvpx_g.a");
    if archive.exists() {
        fs::remove_file(archive)?;
    }

    let mut config = autotools::Config::new(VPX_SRC_DIR);
    for feature in VPX_DISABLED_FEATURES {
        config.disable(feature, None);
    }

    // Build it!
    let dst = config
        .insource(false)
        .forbid("--host")
        .forbid("--build")
        .forbid("--prefix")
        .config_option("target", Some("x86_64-win64-gcc"))
        .cflag(&cflags)
        .env("CC", "clang")
        .env("AR", "llvm-ar")
        .env("AS", "nasm")
        .make_args(vec!["target=libs".into()])
        .make_target("libvpx_g.a")
        .build();

    println!("cargo::rustc-link-search=native={}", dst.join("build").display());
    println!("cargo::rustc-link-lib=static=vpx_g");
    Ok(())
}

fn convert_video(out_dir: &Path) -> Result {
    let source = VIDEO_SOURCES
        .into_iter()
        .find(|path| Path::new(path).exists())
        .ok_or_else(|| format!("No video source found, expected one of {VIDEO_SOURCES:?}"))?;

    // Look for a cache hit first instead of trying to re-encode
    let key = BuildHasherDefault::<DefaultHasher>::new().hash_one((
        fs::read(source)?,
        VP9_ENCODE_ARGS,
        SUPPORTED_PIX_FMTS,
    ));

    let cache_dir = // out dir is usually `<target>/<build-type>/build/<pkg>-<hash>`
        out_dir.ancestors().nth(3).ok_or("unexpected OUT_DIR layout")?.join("video-cache");

    let cached = cache_dir.join(format!("{key:016x}.ivf"));
    if !cached.exists() {
        fs::create_dir_all(&cache_dir)?;

        // Write to a temporary file first, so an interrupt leaves a partial encode as a cache
        let partial = cached.with_extension("ivf.partial");
        transcode(source, &partial)?;
        fs::rename(&partial, &cached)?;
    }

    // Validate the produced file
    let mut header = [0; 12];
    fs::File::open(&cached)?.read_exact(&mut header)?;
    if &header[..4] != b"DKIF" || &header[8..] != b"VP90" {
        return Err("ffmpeg output is not a VP9 IVF file".into());
    }

    fs::copy(&cached, out_dir.join("bad_apple.ivf"))?;
    Ok(())
}

fn transcode(source: &str, output: &Path) -> Result {
    let sh = Shell::new()?;
    let probe = cmd!(sh, "ffprobe -v error -select_streams v:0")
        .args(["-show_entries", "stream=codec_name,pix_fmt", "-of", "csv=p=0", source])
        .quiet()
        .read()?;

    let (codec, pix_fmt) = probe.split_once(',').ok_or("ffprobe found no video stream")?;
    let supported = SUPPORTED_PIX_FMTS.contains(&pix_fmt);

    let codec_args = if codec == "vp9" && supported {
        vec!["-c:v", "copy"]
    } else {
        // Keep the chroma layout when the decoder supports it, otherwise fall back to 4:2:0
        let pix_fmt = if supported { pix_fmt } else { "yuv420p" };
        [&VP9_ENCODE_ARGS[..], &["-pix_fmt", pix_fmt]].concat()
    };

    cmd!(sh, "ffmpeg -hide_banner -loglevel error -i {source} -map 0:v:0 -an {codec_args...}")
        .args(["-f", "ivf", "-y"])
        .arg(output)
        .run()?;

    Ok(())
}
