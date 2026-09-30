//! Framebuffer + E Ink refresh for the Kindle Paperwhite 3 (i.MX6SL, "Wario").
//!
//! The panel is driven by `mxc_epdc_fb` at 8bpp greyscale. Writing pixels into
//! the mapped buffer changes nothing on screen by itself: E Ink only updates
//! when an explicit `MXCFB_SEND_UPDATE` ioctl asks the EPDC controller to
//! redraw a region with a chosen waveform. Drawing and refreshing are therefore
//! separate operations here, deliberately — batching many draws behind one
//! refresh is what keeps the display usable.
//!
//! Struct layouts follow the Lab126 variant of `mxcfb_update_data`, which adds
//! two `hist_*_waveform_mode` fields the mainline i.MX struct does not have.
//! Omitting them misaligns every field after them.

use std::fs::{File, OpenOptions};
use std::io;
use std::os::unix::io::AsRawFd;

// `libc::Ioctl` is the platform's request type: c_int on Linux/arm, c_ulong
// elsewhere. Use the alias rather than pinning a width.
const FBIOGET_VSCREENINFO: libc::Ioctl = 0x4600;
const FBIOGET_FSCREENINFO: libc::Ioctl = 0x4602;

/// `_IOW('F', 0x2E, struct mxcfb_update_data)` with a 72-byte payload.
const MXCFB_SEND_UPDATE: libc::Ioctl = 0x4048_462E;

pub const TEMP_USE_AMBIENT: i32 = 0x1000;

/// Waveform modes, in rough order of "fast and ugly" to "slow and clean".
///
/// The full set is kept even where currently unused: choosing the right
/// waveform per region is the main lever on E Ink responsiveness, and the
/// renderer will need all of them.
#[allow(dead_code)]
#[derive(Clone, Copy, Debug)]
#[repr(u32)]
pub enum Waveform {
    /// Clears the panel to white.
    Init = 0x0,
    /// 2-level, fastest. Good for pen strokes and cursors, leaves ghosting.
    Du = 0x1,
    /// 16-level, high fidelity, visibly flashes. The "clean redraw".
    Gc16 = 0x2,
    /// 16-level, medium fidelity, less flash.
    Gc16Fast = 0x3,
    /// 2-level, fastest of all, heavy ghosting. For transient UI only.
    A2 = 0x4,
    /// High fidelity from a white background — the usual choice for text.
    Gl16 = 0x5,
}

#[allow(dead_code)]
#[derive(Clone, Copy, Debug)]
#[repr(u32)]
pub enum UpdateMode {
    Partial = 0x0,
    Full = 0x1,
}

#[repr(C)]
#[derive(Clone, Copy, Default, Debug)]
pub struct Rect {
    pub top: u32,
    pub left: u32,
    pub width: u32,
    pub height: u32,
}

impl Rect {
    pub fn is_empty(&self) -> bool {
        self.width == 0 || self.height == 0
    }

    /// Smallest rect covering both. Used to coalesce damage: one slightly
    /// larger refresh beats several small ones, because each EPDC update
    /// carries a fixed cost far larger than the extra pixels.
    pub fn union(self, other: Rect) -> Rect {
        if self.is_empty() {
            return other;
        }
        if other.is_empty() {
            return self;
        }
        let left = self.left.min(other.left);
        let top = self.top.min(other.top);
        let right = (self.left + self.width).max(other.left + other.width);
        let bottom = (self.top + self.height).max(other.top + other.height);
        Rect {
            top,
            left,
            width: right - left,
            height: bottom - top,
        }
    }
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct AltBufferData {
    phys_addr: u32,
    width: u32,
    height: u32,
    alt_update_region: Rect,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct UpdateData {
    update_region: Rect,
    waveform_mode: u32,
    update_mode: u32,
    update_marker: u32,
    // Lab126 additions — absent from the mainline i.MX struct.
    hist_bw_waveform_mode: u32,
    hist_gray_waveform_mode: u32,
    temp: i32,
    flags: u32,
    alt_buffer_data: AltBufferData,
}

// The ioctl request number 0x4048462E encodes a 72-byte payload. If this
// struct ever drifts from that, the kernel would read past or short of the
// real fields and misbehave silently, so fail the build instead.
const _: () = assert!(std::mem::size_of::<UpdateData>() == 72);

#[repr(C)]
#[derive(Default)]
struct FbBitfield {
    offset: u32,
    length: u32,
    msb_right: u32,
}

#[repr(C)]
#[derive(Default)]
struct VarScreenInfo {
    xres: u32,
    yres: u32,
    xres_virtual: u32,
    yres_virtual: u32,
    xoffset: u32,
    yoffset: u32,
    bits_per_pixel: u32,
    grayscale: u32,
    red: FbBitfield,
    green: FbBitfield,
    blue: FbBitfield,
    transp: FbBitfield,
    nonstd: u32,
    activate: u32,
    height: u32,
    width: u32,
    accel_flags: u32,
    pixclock: u32,
    left_margin: u32,
    right_margin: u32,
    upper_margin: u32,
    lower_margin: u32,
    hsync_len: u32,
    vsync_len: u32,
    sync: u32,
    vmode: u32,
    rotate: u32,
    colorspace: u32,
    reserved: [u32; 4],
}

#[repr(C)]
struct FixScreenInfo {
    id: [u8; 16],
    smem_start: usize,
    smem_len: u32,
    type_: u32,
    type_aux: u32,
    visual: u32,
    xpanstep: u16,
    ypanstep: u16,
    ywrapstep: u16,
    line_length: u32,
    mmio_start: usize,
    mmio_len: u32,
    accel: u32,
    capabilities: u16,
    reserved: [u16; 2],
}

impl Default for FixScreenInfo {
    fn default() -> Self {
        // Safety: every field is a plain integer or integer array.
        unsafe { std::mem::zeroed() }
    }
}

/// Some accessors below are API surface for the renderer rather than callers
/// today; keep them and quiet the lint instead of trimming and re-adding.
#[allow(dead_code)]
pub struct Framebuffer {
    file: File,
    map: *mut u8,
    map_len: usize,
    pub width: u32,
    pub height: u32,
    pub stride: u32,
    pub bpp: u32,
    pub rotate: u32,
    pub id: String,
    marker: u32,
    /// Bounding box of everything drawn since the last flush.
    damage: Rect,
}

#[allow(dead_code)]
impl Framebuffer {
    pub fn open() -> io::Result<Self> {
        let file = OpenOptions::new().read(true).write(true).open("/dev/fb0")?;
        let fd = file.as_raw_fd();

        let mut var = VarScreenInfo::default();
        let mut fix = FixScreenInfo::default();
        // Safety: both ioctls fill a caller-owned struct of the right type.
        unsafe {
            if libc::ioctl(fd, FBIOGET_VSCREENINFO, &mut var) < 0 {
                return Err(io::Error::last_os_error());
            }
            if libc::ioctl(fd, FBIOGET_FSCREENINFO, &mut fix) < 0 {
                return Err(io::Error::last_os_error());
            }
        }

        let map_len = fix.smem_len as usize;
        // Safety: mapping `smem_len` bytes of the framebuffer the driver reported.
        let map = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                map_len,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_SHARED,
                fd,
                0,
            )
        };
        if map == libc::MAP_FAILED {
            return Err(io::Error::last_os_error());
        }

        let id_end = fix.id.iter().position(|&b| b == 0).unwrap_or(fix.id.len());
        Ok(Framebuffer {
            file,
            map: map as *mut u8,
            map_len,
            width: var.xres,
            height: var.yres,
            stride: fix.line_length,
            bpp: var.bits_per_pixel,
            rotate: var.rotate,
            id: String::from_utf8_lossy(&fix.id[..id_end]).into_owned(),
            marker: 1,
            damage: Rect::default(),
        })
    }

    pub fn map_len(&self) -> usize {
        self.map_len
    }

    /// Whole-screen rectangle, for convenience.
    pub fn full_rect(&self) -> Rect {
        Rect {
            top: 0,
            left: 0,
            width: self.width,
            height: self.height,
        }
    }

    /// Set one pixel. Out-of-bounds writes are dropped rather than panicking,
    /// since drawing code frequently clips at the edges.
    #[inline]
    pub fn set(&mut self, x: u32, y: u32, grey: u8) {
        if x >= self.width || y >= self.height {
            return;
        }
        let off = (y * self.stride + x) as usize;
        if off < self.map_len {
            // Safety: bounds checked against the mapped length just above.
            unsafe { *self.map.add(off) = grey };
            self.damage = self.damage.union(Rect {
                top: y,
                left: x,
                width: 1,
                height: 1,
            });
        }
    }

    /// Read one pixel back out of the mapped buffer.
    ///
    /// Reading costs nothing on E Ink — no panel activity is involved — which
    /// is what makes it practical to poll a region to see whether the Kindle
    /// framework has repainted over it.
    #[inline]
    pub fn get(&self, x: u32, y: u32) -> u8 {
        if x >= self.width || y >= self.height {
            return 0;
        }
        let off = (y * self.stride + x) as usize;
        if off >= self.map_len {
            return 0;
        }
        // Safety: bounds checked against the mapped length just above.
        unsafe { *self.map.add(off) }
    }

    /// Record a region as needing refresh without drawing into it. Useful when
    /// pixels were written through a faster path than `set`.
    pub fn damage(&mut self, r: Rect) {
        self.damage = self.damage.union(r);
    }

    /// Refresh only what changed since the last flush, then clear the damage.
    ///
    /// Returns `None` if nothing was drawn — callers can use that to skip a
    /// pointless EPDC round trip.
    pub fn flush(&mut self, wf: Waveform, mode: UpdateMode) -> io::Result<Option<u32>> {
        let d = self.damage;
        if d.is_empty() {
            return Ok(None);
        }
        self.damage = Rect::default();
        self.refresh(d, wf, mode).map(Some)
    }

    pub fn fill_rect(&mut self, r: Rect, grey: u8) {
        for y in r.top..(r.top + r.height).min(self.height) {
            for x in r.left..(r.left + r.width).min(self.width) {
                self.set(x, y, grey);
            }
        }
    }

    pub fn clear(&mut self, grey: u8) {
        let full = self.full_rect();
        self.fill_rect(full, grey);
    }

    /// Ask the EPDC to actually put `region` on the panel.
    ///
    /// Nothing drawn into the buffer is visible until this runs.
    pub fn refresh(&mut self, region: Rect, wf: Waveform, mode: UpdateMode) -> io::Result<u32> {
        let marker = self.marker;
        self.marker = self.marker.wrapping_add(1).max(1);

        let data = UpdateData {
            update_region: region,
            waveform_mode: wf as u32,
            update_mode: mode as u32,
            update_marker: marker,
            hist_bw_waveform_mode: 0,
            hist_gray_waveform_mode: 0,
            temp: TEMP_USE_AMBIENT,
            flags: 0,
            alt_buffer_data: AltBufferData::default(),
        };

        // Safety: `data` matches the Lab126 mxcfb_update_data layout the
        // 0x4048462E request encodes (72 bytes).
        let rc = unsafe { libc::ioctl(self.file.as_raw_fd(), MXCFB_SEND_UPDATE, &data) };
        if rc < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(marker)
    }
}

impl Drop for Framebuffer {
    fn drop(&mut self) {
        // Safety: unmapping exactly what was mapped in `open`.
        unsafe { libc::munmap(self.map as *mut libc::c_void, self.map_len) };
    }
}
