use std::{
    env,
    fs::{read_to_string, File},
    io::Write as _,
    path::Path,
};

fn main() {
    println!("cargo:rerun-if-changed=ascii.txt");
    println!("cargo:rerun-if-changed=build.rs");

    let out_dir = env::var("OUT_DIR").unwrap();
    let dest_path = Path::new(&out_dir).join("ascii.rs");
    let mut f = File::create(dest_path).unwrap();
    let ascii_file = read_to_string("ascii.txt").expect("failed to read computed ASCII file");
    let frames = ascii_file.split("SPLIT").collect::<Vec<&str>>();

    let _ = f.write_all(format!("static ASCII_FRAMES: &[&str] = &{:?};", frames).as_bytes());
}
