use std::{
    env, fs,
    io::{BufReader, Error, ErrorKind, Read as _, Write},
    path::PathBuf,
    process::{Command, Stdio},
};

const PNG_END: &[u8] = b"\x49\x45\x4E\x44\xAE\x42\x60\x82";
const VIDEO_PATH: &str = "bin/bad_apple.mp4";

fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("cargo::rerun-if-changed={VIDEO_PATH}");
    println!("cargo::rerun-if-changed=build.rs");

    if !fs::exists(VIDEO_PATH).unwrap() {
        println!("cargo::error=Video input file does not exist, acquire a video first!");
        return Err(Box::new(Error::new(ErrorKind::NotFound, VIDEO_PATH)));
    }

    let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap());
    let archive_path = out_dir.join("video_frames.arc");
    let mut archive = fs::File::options()
        .create(true)
        .truncate(true)
        .write(true)
        .open(archive_path)
        .unwrap();

    // Extract frames using `ffmpeg`
    let mut child = Command::new("ffmpeg")
        .arg("-i")
        .arg(VIDEO_PATH)
        .args(["-vsync", "0"])
        .args(["-f", "image2pipe"])
        .args(["-vcodec", "png"])
        .arg("pipe:1")
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .unwrap();

    let stdout = child.stdout.take().unwrap();
    let mut reader = BufReader::new(stdout);

    let mut frame_index = 1;
    let mut buffer = Vec::new();
    let mut chunk = [0u8; 8192];

    // Read frames in a bufferred manner and append them to the archive file
    loop {
        let n = reader.read(&mut chunk).unwrap();
        if n == 0 {
            break; // EOF
        }

        buffer.extend_from_slice(&chunk[..n]);

        while let Some(pos) = buffer.windows(PNG_END.len()).position(|w| w == PNG_END) {
            let png = buffer.drain(..pos + PNG_END.len()).collect::<Vec<u8>>();

            let name = format!("{frame_index}.png");
            let encoded = encode_archive_entry(name, png);

            archive.write_all(&encoded).unwrap();
            archive.flush().unwrap();

            frame_index += 1
        }
    }

    let ffmpeg_status = child.wait().unwrap();
    if !ffmpeg_status.success() {
        let code = ffmpeg_status.code().unwrap_or(1);
        println!("cargo::error=FFmpeg exited with error code: {code}");

        return Err(Box::new(Error::other("frames extraction failed")));
    }

    Ok(())
}

/// Encodes data for a given file name in the archive format.
fn encode_archive_entry(name: String, data: Vec<u8>) -> Vec<u8> {
    let mut encoded = Vec::new();

    // Format: [1-byte filename_length][filename][4-byte data_length][file data]
    encoded.push(name.len() as u8);
    encoded.extend_from_slice(name.as_bytes());
    encoded.extend_from_slice(&(data.len() as u32).to_le_bytes());
    encoded.extend_from_slice(&data);

    encoded
}
