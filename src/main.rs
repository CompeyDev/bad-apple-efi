#![no_main]
#![no_std]

extern crate alloc;
use alloc::vec;
use alloc::vec::Vec;

use uefi::{
    entry, println,
    proto::console::gop::{BltOp, BltPixel, BltRegion, GraphicsOutput},
    table::{Boot, SystemTable},
    Handle, Status,
};

include!(concat!(env!("OUT_DIR"), "/ascii.rs"));

const WIDTH: usize = 300;
const HEIGHT: usize = 240;

#[allow(unreachable_code)]
#[entry]
fn main(_image_handle: Handle, mut system_table: SystemTable<Boot>) -> Status {
    uefi::helpers::init(&mut system_table).unwrap();
    let stdout = system_table.stdout();
    stdout.clear().expect("failed to clear stdout");

    let boot_services = system_table.boot_services();

    let gop_handle = boot_services
        .get_handle_for_protocol::<GraphicsOutput>()
        .expect("failed to get GOP handle");
    let mut gop = boot_services
        .open_protocol_exclusive::<GraphicsOutput>(gop_handle)
        .expect("failed to open GOP");

    let mut modes = gop.modes(boot_services).collect::<Vec<_>>();
    modes.sort_by_key(|x| x.info().resolution());
    let smallest_mode = modes.first().unwrap();
    gop.set_mode(smallest_mode).expect("failed to set GOP mode");

    let (width, height) = gop.current_mode_info().resolution();

    println!("scaled resolution to {width}x{height}");

    let mut default_pixel = BltPixel::new(34, 34, 34);
    for frame in ASCII_FRAMES.iter().take(2180) {
        let mut pixbuf = vec![default_pixel; WIDTH * HEIGHT];
        let frame_matrix = frame
            .split('\n')
            .map(|str| str.as_bytes())
            .collect::<Vec<_>>();

        for (y, x_pixels) in frame_matrix.iter().enumerate() {
            for (x, x_pixel) in (*x_pixels).iter().enumerate() {
                // NOTE: Just provide a placebo pixel so that we don't panic
                // NOTE: `y * WIDTH + x` is just normalizing the matrix indices into a 1D array index
                let real_pixel = pixbuf.get_mut(y * WIDTH + x).unwrap_or(&mut default_pixel);

                // TODO: Handle all the different ASCII chars with different colors
                if *x_pixel == b'$' {
                    // Background, white
                    real_pixel.red = 255;
                    real_pixel.blue = 255;
                    real_pixel.green = 255;
                } else {
                    // Foreground, lighter shade of black
                    real_pixel.red = 34;
                    real_pixel.green = 34;
                    real_pixel.blue = 34;
                }
            }
        }

        gop.blt(BltOp::BufferToVideo {
            buffer: &pixbuf,
            src: BltRegion::Full,
            dest: ((width - WIDTH) / 2, (height - HEIGHT) / 2),
            dims: (WIDTH, HEIGHT),
        })
        .expect("failed to transfer blocks");

        system_table.boot_services().stall(93709);
    }

    boot_services.stall(1_000_000);
    Status::SUCCESS
}
