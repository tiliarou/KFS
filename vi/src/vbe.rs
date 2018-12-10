//! VESA Bios Extensions Framebuffer

use alloc::prelude::*;
use spin::{Mutex, MutexGuard, Once};
use hashmap_core::HashMap;
use syscalls;
use libuser::error::Error;
use core::slice;

/// A rgb color
#[derive(Copy, Clone, Debug)]
#[repr(C)]
pub struct VBEColor {
    b: u8,
    g: u8,
    r: u8,
    a: u8, // Unused
}

/// Some colors for the vbe
impl VBEColor {
    pub fn rgb(r: u8, g: u8, b: u8) -> VBEColor {
        VBEColor {r, g, b, a: 0 }
    }
}

pub struct Framebuffer {
    buf: &'static mut [VBEColor],
    width: usize,
    height: usize,
    bpp: usize
}


impl Framebuffer {
    /// Creates an instance of the linear framebuffer.
    ///
    /// # Safety
    ///
    /// This function should only be called once, to ensure there is only a
    /// single mutable reference to the underlying framebuffer.
    pub fn new() -> Result<Framebuffer, Error> {
        let (buf, width, height, bpp) = syscalls::map_framebuffer()?;

        let mut fb = Framebuffer {
            buf: unsafe { slice::from_raw_parts_mut(buf as *mut _ as *mut _ as *mut VBEColor, buf.len() / 4) },
            width,
            height,
            bpp
        };
        fb.clear();
        Ok(fb)
    }

    /// framebuffer width in pixels. Does not account for bpp
    #[inline]
    pub fn width(&self) -> usize {
        self.width
    }

    /// framebuffer height in pixels. Does not account for bpp
    #[inline]
    pub fn height(&self) -> usize {
        self.height
    }

    /// The number of bits that forms a pixel.
    /// Used to compute offsets in framebuffer memory to corresponding pixel
    /// px_offset = px_nbr * bpp
    #[inline]
    pub fn bpp(&self) -> usize {
        self.bpp
    }

    /// Gets the offset in memory of a pixel based on an x and y.
    ///
    /// # Panics
    ///
    /// Panics if `y >= self.height()` or `x >= self.width()`
    #[inline]
    pub fn get_px_offset(&self, x: usize, y: usize) -> usize {
        assert!(y < self.height());
        assert!(x < self.width());
        (y * self.width() + x)
    }

    /// Writes a pixel in the framebuffer respecting the bgr pattern
    ///
    /// # Panics
    ///
    /// Panics if offset is invalid
    #[inline]
    pub fn write_px(&mut self, offset: usize, color: &VBEColor) {
        self.buf[offset] = *color;
    }

    /// Writes a pixel in the framebuffer respecting the bgr pattern
    /// Computes the offset in the framebuffer from x and y
    ///
    /// # Panics
    ///
    /// Panics if coords are invalid
    #[inline]
    pub fn write_px_at(&mut self, x: usize, y: usize, color: &VBEColor) {
        let offset = self.get_px_offset(x, y);
        self.write_px(offset, color);
    }

    /// Gets the underlying framebuffer
    pub fn get_fb(&mut self) -> &mut [VBEColor] {
        self.buf
    }

    /// Clears the whole screen
    pub fn clear(&mut self) {
        let fb = self.get_fb();
        for i in fb.iter_mut() { *i = VBEColor::rgb(0, 0, 0); }
    }

    /// Clears a segment of the screen.
    ///
    /// # Panics
    ///
    /// Panics if x + width or y + height falls outside the framebuffer.
    pub fn clear_at(&mut self, x: usize, y: usize, width: usize, height: usize) {
        for y in y..y + height {
            for x in x..x + width {
                self.write_px_at(x, y, &VBEColor::rgb(0, 0, 0));
            }
        }
    }
}

lazy_static! {
    pub static ref FRAMEBUFFER: Mutex<Framebuffer> = Mutex::new(Framebuffer::new().unwrap());
}
