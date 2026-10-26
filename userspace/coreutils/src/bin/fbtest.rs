//! `fbtest [SECONDS]`: draw a test picture on `/dev/fb0` (red, green,
//! blue and white quadrants in a white border) and keep it up for SECONDS
//! (default 3), then give the screen back to the console.

#![no_std]
#![no_main]

extern crate alloc;

use libsaeos::fb::Framebuffer;
use libsaeos::prelude::*;
use libsaeos::{Args, entry, time};

entry!(main);

fn main(_args: Args) -> i32 {
    let seconds: u64 = coreutils::arguments()
        .first()
        .and_then(|text| text.parse().ok())
        .unwrap_or(3);
    let mut screen = match Framebuffer::open() {
        Ok(screen) => screen,
        Err(error) => return coreutils::report("fbtest", "/dev/fb0", error),
    };
    let (width, height) = (screen.width, screen.height);
    let (half_width, half_height) = (width / 2, height / 2);
    screen.fill_rect(0, 0, half_width, half_height, 0xff0000);
    screen.fill_rect(half_width, 0, width - half_width, half_height, 0x00ff00);
    screen.fill_rect(0, half_height, half_width, height - half_height, 0x0000ff);
    screen.fill_rect(
        half_width,
        half_height,
        width - half_width,
        height - half_height,
        0xffffff,
    );
    let border = 8;
    screen.fill_rect(0, 0, width, border, 0xffffff);
    screen.fill_rect(0, height - border, width, border, 0xffffff);
    screen.fill_rect(0, 0, border, height, 0xffffff);
    screen.fill_rect(width - border, 0, border, height, 0xffffff);
    time::sleep(seconds * time::NANOS_PER_SECOND);
    drop(screen);
    println!("fbtest: {width}x{height}");
    0
}
