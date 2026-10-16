//! Touchscreen input for the Kindle PW3 (`cyttsp4_mt` on `/dev/input/event1`).
//!
//! The panel speaks the Linux multi-touch protocol, type B: coordinates arrive
//! as a stream of `ABS_MT_*` events and only become a real position when a
//! `SYN_REPORT` closes the packet. Acting on individual axis events instead of
//! waiting for the sync produces phantom half-updated positions.

use std::fs::File;
use std::io::Read;
use std::os::unix::io::AsRawFd;
use std::{io, mem};

const EV_SYN: u16 = 0x00;
const EV_ABS: u16 = 0x03;

const SYN_REPORT: u16 = 0;

const ABS_MT_POSITION_X: u16 = 0x35;
const ABS_MT_POSITION_Y: u16 = 0x36;
const ABS_MT_TRACKING_ID: u16 = 0x39;

/// The kernel's `input_event` as this device lays it out.
///
/// Deliberately NOT built from `libc::timeval`: modern musl uses a 64-bit
/// `time_t` on 32-bit targets, which would make the struct 24 bytes, while this
/// 3.0.35 kernel writes 16. Reading with the wrong size desynchronises the
/// whole event stream and yields plausible-looking garbage coordinates.
#[repr(C)]
#[derive(Clone, Copy, Default, Debug)]
struct InputEvent {
    tv_sec: u32,
    tv_usec: u32,
    kind: u16,
    code: u16,
    value: i32,
}

const _: () = assert!(mem::size_of::<InputEvent>() == 16);

/// `_IOR('E', 0x40 + abs, struct input_absinfo)`, 24-byte payload.
const fn eviocgabs(abs: u16) -> libc::Ioctl {
    (2 << 30) | (24 << 16) | ((b'E' as libc::Ioctl) << 8) | (0x40 + abs as libc::Ioctl)
}

#[repr(C)]
#[derive(Default, Debug)]
pub struct AbsInfo {
    pub value: i32,
    pub minimum: i32,
    pub maximum: i32,
    pub fuzz: i32,
    pub flat: i32,
    pub resolution: i32,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Touch {
    Down { x: i32, y: i32 },
    Move { x: i32, y: i32 },
    Up,
}

pub struct TouchReader {
    file: File,
    buf: [u8; 16],
    // Accumulated across a packet, committed on SYN_REPORT.
    x: i32,
    y: i32,
    tracking: i32,
    was_down: bool,
    dirty: bool,
}

impl TouchReader {
    pub fn open(path: &str) -> io::Result<Self> {
        Ok(TouchReader {
            file: File::open(path)?,
            buf: [0u8; 16],
            x: 0,
            y: 0,
            tracking: -1,
            was_down: false,
            dirty: false,
        })
    }

    /// Query an axis range so coordinates can be mapped to the panel without
    /// hardcoding a guess.
    pub fn abs_info(&self, axis: u16) -> io::Result<AbsInfo> {
        let mut info = AbsInfo::default();
        // Safety: fills a caller-owned 24-byte struct matching the request.
        let rc = unsafe { libc::ioctl(self.file.as_raw_fd(), eviocgabs(axis), &mut info) };
        if rc < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(info)
    }

    pub fn x_range(&self) -> io::Result<AbsInfo> {
        self.abs_info(ABS_MT_POSITION_X)
    }

    pub fn y_range(&self) -> io::Result<AbsInfo> {
        self.abs_info(ABS_MT_POSITION_Y)
    }

    /// Block until the next complete touch packet. `None` means the packet
    /// carried no position change worth reporting.
    pub fn next(&mut self) -> io::Result<Option<Touch>> {
        loop {
            self.file.read_exact(&mut self.buf)?;
            // Safety: buf is exactly 16 bytes and InputEvent is a plain POD of
            // that size (asserted above).
            let ev: InputEvent = unsafe { mem::transmute(self.buf) };

            match (ev.kind, ev.code) {
                (EV_ABS, ABS_MT_POSITION_X) => {
                    self.x = ev.value;
                    self.dirty = true;
                },
                (EV_ABS, ABS_MT_POSITION_Y) => {
                    self.y = ev.value;
                    self.dirty = true;
                },
                (EV_ABS, ABS_MT_TRACKING_ID) => {
                    self.tracking = ev.value;
                    self.dirty = true;
                },
                (EV_SYN, SYN_REPORT) => {
                    if !self.dirty {
                        continue;
                    }
                    self.dirty = false;

                    // tracking_id == -1 is the kernel's "finger lifted".
                    let down = self.tracking >= 0;
                    let out = match (self.was_down, down) {
                        (false, true) => Some(Touch::Down {
                            x: self.x,
                            y: self.y,
                        }),
                        (true, true) => Some(Touch::Move {
                            x: self.x,
                            y: self.y,
                        }),
                        (true, false) => Some(Touch::Up),
                        (false, false) => None,
                    };
                    self.was_down = down;
                    if out.is_some() {
                        return Ok(out);
                    }
                },
                _ => {},
            }
        }
    }
}
