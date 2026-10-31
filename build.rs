use std::hash::{BuildHasher, BuildHasherDefault, DefaultHasher};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::{env, fs};

use xshell::{cmd, Shell};

const VIDEO_SOURCES: [&str; 2] = ["bin/bad_apple.webm", "bin/bad_apple.mp4"];
const VP9_ENCODE_ARGS: [&str; 10] =
    ["-c:v", "libvpx-vp9", "-crf", "37", "-b:v", "0", "-cpu-used", "2", "-row-mt", "1"];
// Opus always decodes at 48 kHz, which HDA plays without resampling
const OPUS_ENCODE_ARGS: [&str; 6] = ["-c:a", "libopus", "-b:a", "128k", "-ar", "48000"];

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

const UHDA_SRC_DIR: &str = "vendor/uHDA";
const UHDA_PATCHES_DIR: &str = "vendor/patches/uHDA";
const UHDA_SOURCES: [&str; 5] =
    ["uhda.cpp", "controller.cpp", "codec.cpp", "stream.cpp", "simple.cpp"];

const OPUS_SRC_DIR: &str = "vendor/opus";
const OPUS_SOURCE_LISTS: [(&str, &str); 3] = [
    ("celt_sources.mk", "CELT_SOURCES"),
    ("silk_sources.mk", "SILK_SOURCES"),
    ("opus_sources.mk", "OPUS_SOURCES"),
];

type Result<T = ()> = std::result::Result<T, Box<dyn std::error::Error>>;

fn main() -> Result {
    println!("cargo::rerun-if-changed={FREESTANDING_DIR}");
    println!("cargo::rerun-if-changed=bin");

    let out_dir = PathBuf::from(env::var("OUT_DIR")?);
    build_libvpx(&out_dir)?;
    build_uhda(&out_dir)?;
    build_opus()?;
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

fn build_uhda(out_dir: &Path) -> Result {
    println!("cargo::rerun-if-changed={UHDA_SRC_DIR}");
    println!("cargo::rerun-if-changed={UHDA_PATCHES_DIR}");

    // Apply our own patches
    let patched = patch_uhda(out_dir)?;
    let source = |path: PathBuf| match patched.join(&path) {
        patched if patched.exists() => patched,
        _ => Path::new(UHDA_SRC_DIR).join(path),
    };

    // Build it!
    cc::Build::new()
        .cpp(true)
        .compiler("clang++")
        .archiver("llvm-ar")
        .target("x86_64-pc-windows-msvc")
        .std("c++20")
        .files(UHDA_SOURCES.map(|file| source(Path::new("src").join(file))))
        .includes([patched.join("include"), Path::new(UHDA_SRC_DIR).join("include")])
        .include(Path::new(UHDA_SRC_DIR).join("src"))
        .flag("-ffreestanding")
        .flag("-fno-exceptions")
        .flag("-fno-rtti")
        .cpp_link_stdlib(None)
        .compile("uhda");

    Ok(())
}

fn build_opus() -> Result {
    println!("cargo::rerun-if-changed={OPUS_SRC_DIR}");

    let mut sources = Vec::new();
    for (makefile, list) in OPUS_SOURCE_LISTS {
        // Lists look like `NAME = \` followed by a file per line, until a blank line
        let text = fs::read_to_string(Path::new(OPUS_SRC_DIR).join(makefile))?;
        let files = text
            .lines()
            .skip_while(|line| line.split_once('=').is_none_or(|(name, _)| name.trim() != list))
            .skip(1)
            .map(|line| line.trim_end_matches('\\').trim())
            .take_while(|file| !file.is_empty());

        sources.extend(files.map(|file| Path::new(OPUS_SRC_DIR).join(file)));
    }

    // Build it!
    let opus = Path::new(OPUS_SRC_DIR);
    cc::Build::new()
        .compiler("clang")
        .archiver("llvm-ar")
        .target("x86_64-pc-windows-msvc")
        .files(sources)
        .include(FREESTANDING_DIR)
        .includes(["include", "celt", "silk", "silk/fixed"].map(|dir| opus.join(dir)))
        .define("OPUS_BUILD", None)
        .define("FIXED_POINT", None)
        .define("DISABLE_FLOAT_API", None)
        .define("VAR_ARRAYS", None)
        .define("NO_ASSERTS", None) // SILK would use MSVC's debug asserts otherwise
        .flag("-ffreestanding")
        .flag("-mavx2")
        .flag("-mfma")
        .warnings(false)
        .compile("opus");

    Ok(())
}

// TODO: make patching universal for all vendored code
fn patch_uhda(out_dir: &Path) -> Result<PathBuf> {
    // Start fresh, in case patches were removed, we don't want them applied already
    let patched = out_dir.join("uhda-patched");
    if patched.exists() {
        fs::remove_dir_all(&patched)?;
    }

    let mut patches = fs::read_dir(UHDA_PATCHES_DIR)?.collect::<std::io::Result<Vec<_>>>()?;
    patches.sort_by_key(|entry| entry.file_name());

    for path in patches.iter().map(|entry| entry.path()) {
        let text = fs::read_to_string(&path)?;
        let patch = diffy::Patch::from_str(&text)
            .map_err(|err| format!("invalid patch {}: {err}", path.display()))?;

        let target = patch
            .modified()
            .and_then(|name| name.strip_prefix("b/"))
            .ok_or_else(|| format!("{} has no target file", path.display()))?;

        // Later patches to the same file apply on top of the earlier ones
        let dest = patched.join(target);
        let source = fs::read_to_string(if dest.exists() {
            dest.clone()
        } else {
            Path::new(UHDA_SRC_DIR).join(target)
        })?;

        let patched = diffy::apply(&source, &patch)
            .map_err(|err| format!("{} couldn't apply to uHDA: {err}", path.display()))?;

        fs::create_dir_all(dest.parent().ok_or("patch target has no parent")?)?;
        fs::write(dest, patched)?;
    }

    Ok(patched)
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
        OPUS_ENCODE_ARGS,
        SUPPORTED_PIX_FMTS,
    ));

    let cache_dir = // out dir is usually `<target>/<build-type>/build/<pkg>-<hash>`
        out_dir.ancestors().nth(3).ok_or("unexpected OUT_DIR layout")?.join("video-cache");

    let cached = cache_dir.join(format!("{key:016x}.webm"));
    if !cached.exists() {
        fs::create_dir_all(&cache_dir)?;

        // Write to a temporary file first, so an interrupt leaves a partial encode as a cache
        let partial = cached.with_extension("webm.partial");
        transcode(source, &partial)?;
        fs::rename(&partial, &cached)?;
    }

    // Validate the produced file, the DocType sits within the first few bytes of the EBML header
    let mut header = [0; 40];
    fs::File::open(&cached)?.read_exact(&mut header)?;
    if header[..4] != [0x1A, 0x45, 0xDF, 0xA3] || !header.windows(4).any(|w| w == b"webm") {
        return Err("ffmpeg output is not a WebM file".into());
    }

    fs::copy(&cached, out_dir.join("bad_apple.webm"))?;
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

    let video_args = if codec == "vp9" && supported {
        vec!["-c:v", "copy"]
    } else {
        // Keep the chroma layout when the decoder supports it, otherwise fall back to 4:2:0
        let pix_fmt = if supported { pix_fmt } else { "yuv420p" };
        [&VP9_ENCODE_ARGS[..], &["-pix_fmt", pix_fmt]].concat()
    };

    let probe = cmd!(sh, "ffprobe -v error -select_streams a:0")
        .args(["-show_entries", "stream=codec_name,sample_rate", "-of", "csv=p=0", source])
        .quiet()
        .read()?;

    // The audio track is optional, playback is silent when absent
    let audio_args = match probe.split_once(',') {
        None => vec!["-an"],
        Some(("opus", "48000")) => vec!["-map", "0:a:0", "-c:a", "copy"],
        Some(_) => [&["-map", "0:a:0"][..], &OPUS_ENCODE_ARGS].concat(),
    };

    cmd!(sh, "ffmpeg -hide_banner -loglevel error -i {source} -map 0:v:0 {video_args...}")
        .args(audio_args)
        .args(["-f", "webm", "-y"])
        .arg(output)
        .run()?;

    Ok(())
}
