#![no_main]
#![no_std]

extern crate alloc;
use alloc::vec;
use alloc::vec::Vec;

use uefi::{
    entry,
    proto::console::gop::{BltOp, BltPixel, BltRegion, GraphicsOutput},
    table::{Boot, SystemTable},
    Handle, Status,
};

include!(concat!(env!("OUT_DIR"), "/ascii.rs"));

const WIDTH: usize = 300;
const HEIGHT: usize = 90;

#[allow(unreachable_code)]
#[entry]
fn main(_image_handle: Handle, mut system_table: SystemTable<Boot>) -> Status {
    uefi::helpers::init(&mut system_table).unwrap();
    system_table
        .stdout()
        .clear()
        .expect("failed to clear stdio");

    let boot_services = system_table.boot_services();

    let gop_handle = boot_services
        .get_handle_for_protocol::<GraphicsOutput>()
        .expect("failed to get GOP handle");
    let mut gop = boot_services
        .open_protocol_exclusive::<GraphicsOutput>(gop_handle)
        .expect("failed to open GOP");

    let mut frame_pos = 0;
    while frame_pos <= 2180 {
        let mut pixbuf = vec![BltPixel::new(0, 0, 0); WIDTH * HEIGHT];
        let frame = ASCII_FRAMES[frame_pos];
        let dimensional_frame = frame.as_bytes().windows(WIDTH).collect::<Vec<_>>();

        for (y, x_pixels) in dimensional_frame.iter().enumerate() {
            for (x, x_pixel) in (*x_pixels).iter().enumerate() {
                let idx = x * y + (WIDTH - x);
                let real_pixel = match pixbuf.get_mut(idx) {
                    Some(inner) => inner,
                    None => continue,
                };

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
            dest: (0, 0),
            dims: (WIDTH, HEIGHT),
        })
        .expect("failed to transfer blocks");

        system_table.boot_services().stall(93709);
        frame_pos += 1;
    }

    boot_services.stall(1_000_000);

    Status::SUCCESS
}
