use std::hash::{BuildHasher, BuildHasherDefault, DefaultHasher};
use std::path::{Path, PathBuf};
use std::{env, fs};

use anyhow::{ensure, Context};
use flickzeug::fs::{apply_patch_dir, DirApplyOptions, FileOutcome};
use flickzeug::Patch;
use xshell::{cmd, Shell};

const VENDOR_DIR: &str = "vendor";
const PATCHES_DIR: &str = "vendor/patches";
const FREESTANDING_DIR: &str = "vendor/freestanding";

type Result<T = ()> = anyhow::Result<T>;

fn main() -> Result {
    println!("cargo::rerun-if-changed={FREESTANDING_DIR}");

    let sh = Shell::new()?;
    let out_dir = PathBuf::from(env::var("OUT_DIR")?);

    // Compile and link vendored C libraries
    libvpx::build(&sh, &out_dir)?;
    uhda::build(&sh, &out_dir)?;
    opus::build(&sh)?;

    // Encode and embed the video
    video::build(&sh, &out_dir)
}

mod libvpx {
    use super::*;

    const DISABLED_FEATURES: [&str; 10] = [
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

    pub fn build(sh: &Shell, out_dir: &Path) -> Result {
        let src = Source::vendored("libvpx");

        // Include the freestanding dir as the first path, so it wins over the generated config
        // header file when possible
        let freestanding = fs::canonicalize(FREESTANDING_DIR)?;
        let cflags = format!(
            "-target x86_64-windows-msvc -mavx2 -mfma -ffreestanding -I{}",
            freestanding.display()
        );

        // Old archives are never dropped even if the new config no longer needs them, we
        // do it manually
        sh.remove_path(out_dir.join("build/libvpx_g.a"))?;

        let mut config = autotools::Config::new(&src);
        for feature in DISABLED_FEATURES {
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
            .env("LD", "clang -target x86_64-windows-msvc -fuse-ld=lld -nostdlib -Wl,-entry:main")
            .make_args(vec!["target=libs".into()])
            .make_target("libvpx_g.a")
            .build();

        println!("cargo::rustc-link-search=native={}", dst.join("build").display());
        println!("cargo::rustc-link-lib=static=vpx_g");
        Ok(())
    }
}

mod uhda {
    use super::*;

    const SOURCES: [&str; 5] =
        ["uhda.cpp", "controller.cpp", "codec.cpp", "stream.cpp", "simple.cpp"];

    pub fn build(sh: &Shell, out_dir: &Path) -> Result {
        let src = Source::patched(sh, "uHDA", out_dir)?;

        // Build it!
        cc::Build::new()
            .freestanding()
            .cpp(true)
            .compiler("clang++")
            .std("c++20")
            .files(SOURCES.map(|file| src.join("src").join(file)))
            .includes([src.join("include"), src.join("src")])
            .flags(["-fno-exceptions", "-fno-rtti"])
            .cpp_link_stdlib(None)
            .compile("uhda");

        Ok(())
    }
}

mod opus {
    use super::*;

    pub fn build(sh: &Shell) -> Result {
        let src = Source::vendored("opus");

        // Let make expand the source lists, which each library has a makefile for
        let sources = {
            let _dir = sh.push_dir(&src);
            cmd!(sh, "make -s -f celt_sources.mk -f silk_sources.mk -f opus_sources.mk")
                .arg("--eval=print: ; @echo $(CELT_SOURCES) $(SILK_SOURCES) $(OPUS_SOURCES)")
                .arg("print")
                .read()?
        };

        // Build it!
        cc::Build::new()
            .freestanding()
            .files(sources.split_whitespace().map(|file| src.join(file)))
            .include(FREESTANDING_DIR)
            .includes(["include", "celt", "silk", "silk/fixed"].map(|dir| src.join(dir)))
            .flags(["-DOPUS_BUILD", "-DFIXED_POINT", "-DDISABLE_FLOAT_API", "-DVAR_ARRAYS"])
            .define("NO_ASSERTS", None) // SILK would use MSVC's debug asserts otherwise
            .flags(["-mavx2", "-mfma"])
            .warnings(false)
            .compile("opus");

        Ok(())
    }
}

/// Vendored sources to build from.
struct Source;

impl Source {
    /// The source at `vendor/<name>`, as it is.
    fn vendored(name: &str) -> PathBuf {
        let dir = Path::new(VENDOR_DIR).join(name);
        println!("cargo::rerun-if-changed={}", dir.display());
        dir
    }

    /// A copy of the source at `vendor/<name>` in `out_dir`, with our patches at
    /// `vendor/patches/<name>` applied.
    fn patched(sh: &Shell, name: &str, out_dir: &Path) -> Result<PathBuf> {
        let source = Self::vendored(name);
        let patches = Path::new(PATCHES_DIR).join(name);
        println!("cargo::rerun-if-changed={}", patches.display());

        // Start fresh, in case patches were removed, and check out the whole source, so patched
        // files never include unpatched ones
        let patched = out_dir.join("patched").join(name);
        sh.remove_path(&patched)?;

        cmd!(sh, "git -C {source} checkout-index --all")
            .arg(format!("--prefix={}/", patched.display()))
            .run()?;

        // Patches are made against the exact source, so any mismatch means the source has changed,
        // we should abort the patching and raise an error
        let mut options = DirApplyOptions { write_rejects: false, ..Default::default() };
        options.apply_config.fuzzy_config.max_fuzz = 0;
        options.apply_config.fuzzy_config.similarity_threshold = 1.0;

        for path in sh.read_dir(patches)? {
            let text = fs::read(&path)?;
            let patch = Patch::from_bytes(&text)
                .with_context(|| format!("{} is not a valid patch", path.display()))?;

            for file in apply_patch_dir(&patched, &patch, &options)? {
                let applied = matches!(file.outcome, FileOutcome::Patched { .. });
                ensure!(applied, "{} didn't apply: {file:?}", path.display());
            }
        }

        Ok(patched)
    }
}

/// Our toolchain for freestanding code, which C and C++ sources share.
trait Freestanding {
    fn freestanding(&mut self) -> &mut Self;
}

impl Freestanding for cc::Build {
    fn freestanding(&mut self) -> &mut Self {
        self.compiler("clang")
            .archiver("llvm-ar")
            .target("x86_64-pc-windows-msvc")
            .flag("-ffreestanding")
    }
}

mod video {
    use super::*;

    const SOURCES: [&str; 2] = ["bin/bad_apple.webm", "bin/bad_apple.mp4"];
    const SUPPORTED_PIX_FMTS: [&str; 5] = ["yuv420p", "yuv422p", "yuv440p", "yuv444p", "gbrp"];
    const VP9_ENCODE_ARGS: [&str; 10] =
        ["-c:v", "libvpx-vp9", "-crf", "37", "-b:v", "0", "-cpu-used", "2", "-row-mt", "1"];

    // Opus always decodes at 48 kHz, which HDA plays without resampling
    const OPUS_ENCODE_ARGS: [&str; 6] = ["-c:a", "libopus", "-b:a", "128k", "-ar", "48000"];

    /// Converts the video into a WebM we can play, cached across builds since encoding is slow.
    pub fn build(sh: &Shell, out_dir: &Path) -> Result {
        println!("cargo::rerun-if-changed=bin");

        let source = SOURCES.into_iter().find(|path| Path::new(path).exists());
        let source = source
            .with_context(|| format!("No video source found, expected one of {SOURCES:?}"))?;

        // Look for a cache hit first instead of trying to re-encode, out dir is usually
        // `<target>/<build-type>/build/<pkg>-<hash>/out`
        let data = fs::read(source)?;
        let hasher = BuildHasherDefault::<DefaultHasher>::new();
        let key = hasher.hash_one((data, VP9_ENCODE_ARGS, OPUS_ENCODE_ARGS, SUPPORTED_PIX_FMTS));

        let target = out_dir.ancestors().nth(3).context("unexpected OUT_DIR layout")?;
        let cache_dir = target.join("video-cache");
        let cached = cache_dir.join(format!("{key:016x}.webm"));

        if !sh.path_exists(&cached) {
            sh.create_dir(&cache_dir)?;

            // Write to a temporary file first, so an interrupt leaves a partial encode as a cache
            let partial = cached.with_extension("webm.partial");
            transcode(sh, source, &partial)?;
            fs::rename(&partial, &cached)?;
        }

        // Validate the produced file
        let format = cmd!(sh, "ffprobe -v error -show_entries format=format_name")
            .args(["-of", "default=nw=1:nk=1"])
            .arg(&cached)
            .quiet()
            .read()?;
        ensure!(format.split(',').any(|name| name == "webm"), "ffmpeg output is not a WebM file");

        sh.copy_file(&cached, out_dir.join("bad_apple.webm"))?;
        Ok(())
    }

    /// Encodes `source` into `output`, copying the streams which are already playable.
    fn transcode(sh: &Shell, source: &str, output: &Path) -> Result {
        let video = probe(sh, source, "v:0", "codec_name,pix_fmt")?;
        let (codec, pix_fmt) = video.split_once(',').context("ffprobe found no video stream")?;
        let supported = SUPPORTED_PIX_FMTS.contains(&pix_fmt);

        let video_args = if codec == "vp9" && supported {
            vec!["-c:v", "copy"]
        } else {
            // Keep the chroma layout when the decoder supports it, otherwise fall back to 4:2:0
            let pix_fmt = if supported { pix_fmt } else { "yuv420p" };
            [&VP9_ENCODE_ARGS[..], &["-pix_fmt", pix_fmt]].concat()
        };

        // The audio track is optional, playback is silent when absent
        let audio = probe(sh, source, "a:0", "codec_name,sample_rate")?;
        let audio_args = match audio.split_once(',') {
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

    /// The `entries` of a `stream` in `source`, separated by commas, or nothing without one.
    fn probe(sh: &Shell, source: &str, stream: &str, entries: &str) -> Result<String> {
        let entries = format!("stream={entries}");
        let probe = cmd!(sh, "ffprobe -v error -select_streams {stream} -show_entries {entries}")
            .args(["-of", "csv=p=0", source])
            .quiet()
            .read()?;

        Ok(probe)
    }
}
