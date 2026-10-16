//! Magician thin client for the Kindle Paperwhite 3.
//!
//!   magkindle pattern   test pattern, full GC16 refresh (default)
//!   magkindle touch     report touch axis ranges and live events
//!   magkindle paint     draw under your finger using fast partial refresh
//!   magkindle restore   hand the screen back to the Kindle UI
//!   magkindle ribbon    show a DEV badge (add `watch` to keep it up, `off` to clear)
//!   magkindle fonts     list TrueType fonts available on the device
//!   magkindle text      render real antialiased text (optional: font path)

mod fb;
mod font;
mod input;
mod text;

use fb::{Framebuffer, Rect, UpdateMode, Waveform};
use input::{Touch, TouchReader};

const TOUCH_DEV: &str = "/dev/input/event1";

/// Hand the screen back to the Kindle framework.
///
/// Drawing straight into `/dev/fb0` bypasses the framework entirely, so it has
/// no idea the screen changed and will happily leave our pixels up forever.
/// Without this, exiting any of these modes strands the device showing whatever
/// we last drew, which reads as a hang. Asking appmgrd to re-open the home
/// booklet forces a full repaint.
fn restore_ui() {
    let _ = std::process::Command::new("lipc-set-prop")
        .args([
            "com.lab126.appmgrd",
            "start",
            "app://com.lab126.booklet.home",
        ])
        .status();
}

fn main() -> std::io::Result<()> {
    let mode = std::env::args().nth(1).unwrap_or_else(|| "pattern".into());
    match mode.as_str() {
        "pattern" => pattern(),
        "touch" => touch_probe(),
        "paint" => paint(),
        "restore" => restore(),
        "ribbon" => ribbon(std::env::args().nth(2)),
        "fonts" => fonts(),
        "text" => text_demo(std::env::args().nth(2)),
        other => {
            eprintln!("unknown mode: {other}");
            eprintln!("usage: magkindle [pattern|touch|paint|restore|ribbon|fonts|text]");
            std::process::exit(2);
        },
    }
}

fn pattern() -> std::io::Result<()> {
    let mut fb = Framebuffer::open()?;
    println!(
        "device      : {} ({}x{}, {}bpp, stride {})",
        fb.id, fb.width, fb.height, fb.bpp, fb.stride
    );

    fb.clear(0xFF);
    let (w, h) = (fb.width, fb.height);
    let t = 8;
    fb.fill_rect(
        Rect {
            top: 0,
            left: 0,
            width: w,
            height: t,
        },
        0x00,
    );
    fb.fill_rect(
        Rect {
            top: h - t,
            left: 0,
            width: w,
            height: t,
        },
        0x00,
    );
    fb.fill_rect(
        Rect {
            top: 0,
            left: 0,
            width: t,
            height: h,
        },
        0x00,
    );
    fb.fill_rect(
        Rect {
            top: 0,
            left: w - t,
            width: t,
            height: h,
        },
        0x00,
    );

    let steps = 8u32;
    let band_h = (h - 4 * t) / steps;
    for i in 0..steps {
        let grey = (i * 255 / (steps - 1)) as u8;
        fb.fill_rect(
            Rect {
                top: 2 * t + i * band_h,
                left: 2 * t,
                width: w - 4 * t,
                height: band_h,
            },
            grey,
        );
    }

    fb.refresh(fb.full_rect(), Waveform::Gc16, UpdateMode::Full)?;
    println!("refresh     : ok (full GC16)");
    println!();
    println!("the pattern stays up on purpose — inspect it.");
    println!("run `magkindle restore` to hand the screen back to the Kindle.");
    Ok(())
}

/// Explicitly give the screen back, for when a mode left something up.
///
/// Asking appmgrd to go home is not enough on its own: the framework repaints
/// only the regions it owns, so anything we drew outside them survives as
/// artifacts in the gaps. Clear to white with a full GC16 first, then hand
/// back — that leaves nothing of ours behind.
fn restore() -> std::io::Result<()> {
    match Framebuffer::open() {
        Ok(mut fb) => {
            fb.clear(0xFF);
            fb.refresh(fb.full_rect(), Waveform::Gc16, UpdateMode::Full)?;
        },
        Err(e) => eprintln!("could not clear the framebuffer: {e}"),
    }
    restore_ui();
    println!("cleared and handed the screen back to the Kindle UI");
    Ok(())
}

/// Report the panel-to-touch coordinate relationship rather than assuming it.
fn touch_probe() -> std::io::Result<()> {
    let fb = Framebuffer::open()?;
    let mut touch = TouchReader::open(TOUCH_DEV)?;

    let xr = touch.x_range()?;
    let yr = touch.y_range()?;
    println!("panel       : {}x{}", fb.width, fb.height);
    println!("touch X     : {}..{}", xr.minimum, xr.maximum);
    println!("touch Y     : {}..{}", yr.minimum, yr.maximum);
    println!();
    println!("tap the screen (20 events, then exits)");
    println!("watch whether X tracks the SHORT edge and Y the LONG edge");
    println!();

    for i in 0..20 {
        match touch.next()? {
            Some(Touch::Down { x, y }) => println!("{i:2}  down  x={x:5} y={y:5}"),
            Some(Touch::Move { x, y }) => println!("{i:2}  move  x={x:5} y={y:5}"),
            Some(Touch::Up) => println!("{i:2}  up"),
            None => {},
        }
    }
    Ok(())
}

/// Paint under the finger. Exercises the thing that makes E Ink usable:
/// small, fast, partial refreshes instead of full-screen redraws.
fn paint() -> std::io::Result<()> {
    let mut fb = Framebuffer::open()?;
    let mut touch = TouchReader::open(TOUCH_DEV)?;

    let xr = touch.x_range()?;
    let yr = touch.y_range()?;

    fb.clear(0xFF);
    fb.refresh(fb.full_rect(), Waveform::Gc16, UpdateMode::Full)?;
    println!("paint: draw with a finger. 300 events then exits.");
    println!(
        "touch X {}..{}  Y {}..{}",
        xr.minimum, xr.maximum, yr.minimum, yr.maximum
    );

    // Map the digitiser range onto the panel. Derived from the reported axis
    // ranges rather than hardcoded, so a different panel still lands correctly.
    let map = |v: i32, lo: i32, hi: i32, out: u32| -> u32 {
        let span = (hi - lo).max(1);
        (((v - lo).clamp(0, span) as i64 * (out.saturating_sub(1)) as i64) / span as i64) as u32
    };

    let r = 6u32;
    for _ in 0..300 {
        let (x, y) = match touch.next()? {
            Some(Touch::Down { x, y }) | Some(Touch::Move { x, y }) => (x, y),
            Some(Touch::Up) => {
                // Settle the stroke with a cleaner waveform on lift.
                fb.flush(Waveform::Gl16, UpdateMode::Partial)?;
                continue;
            },
            None => continue,
        };

        let px = map(x, xr.minimum, xr.maximum, fb.width);
        let py = map(y, yr.minimum, yr.maximum, fb.height);

        fb.fill_rect(
            Rect {
                top: py.saturating_sub(r),
                left: px.saturating_sub(r),
                width: r * 2,
                height: r * 2,
            },
            0x00,
        );

        // A2 is the fastest waveform: 2-level, ghosty, but it keeps the stroke
        // under the finger instead of lagging behind it.
        fb.flush(Waveform::A2, UpdateMode::Partial)?;
    }

    // Clear before handing back: the framework repaints only its own regions,
    // so leftover strokes would survive in the gaps.
    fb.clear(0xFF);
    fb.refresh(fb.full_rect(), Waveform::Gc16, UpdateMode::Full)?;
    restore_ui();
    println!("done — screen cleared and handed back to the Kindle UI");
    Ok(())
}

// ---------------------------------------------------------------------------
// Daemonising
// ---------------------------------------------------------------------------

const RIBBON_PIDFILE: &str = "/tmp/magkindle-ribbon.pid";

/// Detach fully from the parent so the process survives its launcher.
///
/// `nohup ... &` is not enough here: the scriptlet runner reaps its whole
/// process group on exit, which silently killed the watcher a moment after it
/// started. The standard double-fork plus `setsid` puts us in a new session
/// with no controlling terminal, where nothing can sweep us up.
fn daemonize() -> std::io::Result<()> {
    // Safety: textbook double-fork; each fork's parent exits immediately and
    // only the grandchild returns to the caller.
    unsafe {
        match libc::fork() {
            -1 => return Err(std::io::Error::last_os_error()),
            0 => {},
            _ => std::process::exit(0),
        }
        if libc::setsid() < 0 {
            return Err(std::io::Error::last_os_error());
        }
        // Second fork: a session leader could reacquire a terminal, this
        // grandchild never can.
        match libc::fork() {
            -1 => return Err(std::io::Error::last_os_error()),
            0 => {},
            _ => std::process::exit(0),
        }
        // Our stdio belongs to a launcher that is about to disappear.
        let devnull = libc::open(c"/dev/null".as_ptr(), libc::O_RDWR);
        if devnull >= 0 {
            libc::dup2(devnull, 0);
            libc::dup2(devnull, 1);
            libc::dup2(devnull, 2);
            if devnull > 2 {
                libc::close(devnull);
            }
        }
    }
    let _ = std::fs::write(RIBBON_PIDFILE, format!("{}\n", std::process::id()));
    Ok(())
}

/// Stop a running watcher, if any. Returns true if one was signalled.
fn stop_ribbon_watcher() -> bool {
    let Ok(txt) = std::fs::read_to_string(RIBBON_PIDFILE) else {
        return false;
    };
    let Ok(pid) = txt.trim().parse::<i32>() else {
        return false;
    };
    // Safety: SIGTERM to a pid we recorded ourselves.
    let rc = unsafe { libc::kill(pid, libc::SIGTERM) };
    let _ = std::fs::remove_file(RIBBON_PIDFILE);
    rc == 0
}

// ---------------------------------------------------------------------------
// DEV ribbon
// ---------------------------------------------------------------------------

/// Badge geometry: a black tab in the top-right with "DEV" reversed out.
const RIBBON_SCALE: u32 = 4;
const RIBBON_PAD: u32 = 10;
const RIBBON_INSET: u32 = 6;

fn ribbon_rect(fb: &Framebuffer) -> Rect {
    let w = text::text_width("DEV", RIBBON_SCALE) + RIBBON_PAD * 2;
    let h = text::GLYPH_H * RIBBON_SCALE + RIBBON_PAD * 2;
    Rect {
        top: RIBBON_INSET,
        left: fb.width.saturating_sub(w + RIBBON_INSET),
        width: w,
        height: h,
    }
}

fn draw_ribbon(fb: &mut Framebuffer) -> Rect {
    let r = ribbon_rect(fb);
    fb.fill_rect(r, 0x00);
    text::draw(
        fb,
        r.left + RIBBON_PAD,
        r.top + RIBBON_PAD,
        RIBBON_SCALE,
        "DEV",
        0xFF,
    );
    r
}

/// Has the framework painted over us?
///
/// Sampling the black border rather than the glyphs: the border is a solid
/// block, so a handful of points is a reliable signal without scanning the
/// whole badge every poll.
fn ribbon_intact(fb: &Framebuffer, r: Rect) -> bool {
    let pts = [
        (r.left + 2, r.top + 2),
        (r.left + r.width - 3, r.top + 2),
        (r.left + 2, r.top + r.height - 3),
        (r.left + r.width - 3, r.top + r.height - 3),
    ];
    pts.iter().all(|&(x, y)| fb.get(x, y) == 0x00)
}

fn ribbon(arg: Option<String>) -> std::io::Result<()> {
    let mut fb = Framebuffer::open()?;
    let r = ribbon_rect(&fb);

    match arg.as_deref() {
        Some("off") => {
            let killed = stop_ribbon_watcher();
            fb.fill_rect(r, 0xFF);
            fb.refresh(r, Waveform::Gc16, UpdateMode::Partial)?;
            restore_ui();
            println!(
                "ribbon cleared (watcher {})",
                if killed { "stopped" } else { "not running" }
            );
            return Ok(());
        },
        Some("watch") => {},
        None => {
            draw_ribbon(&mut fb);
            fb.refresh(r, Waveform::Gc16, UpdateMode::Partial)?;
            println!("ribbon drawn at {r:?}");
            return Ok(());
        },
        Some(other) => {
            eprintln!("unknown ribbon arg: {other} (want: watch|off)");
            std::process::exit(2);
        },
    }

    // Watch mode. Anything the framework repaints wipes the badge, so redraw
    // it whenever it disappears. Reading the framebuffer is free; only the
    // redraw costs a panel update, so an idle device stays completely quiet.
    stop_ribbon_watcher();
    println!("ribbon watch: detaching; will redraw whenever the framework paints over it");

    // Drop the framebuffer before forking and reopen in the child: an mmap and
    // an open fd shared across a fork is asking for confusion later.
    drop(fb);
    daemonize()?;
    let mut fb = Framebuffer::open()?;

    draw_ribbon(&mut fb);
    fb.refresh(r, Waveform::Gc16, UpdateMode::Partial)?;

    loop {
        std::thread::sleep(std::time::Duration::from_millis(1500));
        if !ribbon_intact(&fb, r) {
            draw_ribbon(&mut fb);
            // A2 is the cheap 2-level waveform: the badge is pure black and
            // white, so there is nothing for a richer waveform to add.
            let _ = fb.refresh(r, Waveform::A2, UpdateMode::Partial);
        }
    }
}

// ---------------------------------------------------------------------------
// Text
// ---------------------------------------------------------------------------

/// Places the Kindle keeps fonts. The device ships real reading faces, so
/// there is no reason to embed one and pay for it in every binary.
const FONT_DIRS: &[&str] = &[
    "/usr/java/lib/fonts",
    "/mnt/us/fonts",
    "/usr/share/fonts",
    "/mnt/us/linkfonts/fonts",
];

fn find_fonts() -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    for dir in FONT_DIRS {
        let Ok(entries) = std::fs::read_dir(dir) else {
            continue;
        };
        for e in entries.flatten() {
            let p = e.path();
            let ext = p
                .extension()
                .and_then(|s| s.to_str())
                .unwrap_or("")
                .to_ascii_lowercase();
            if ext == "ttf" || ext == "otf" {
                out.push(p);
            }
        }
    }
    out.sort();
    out
}

fn fonts() -> std::io::Result<()> {
    let found = find_fonts();
    if found.is_empty() {
        println!("no TrueType fonts found in:");
        for d in FONT_DIRS {
            println!("  {d}");
        }
        return Ok(());
    }
    println!("{} font(s):", found.len());
    for p in &found {
        let size = std::fs::metadata(p).map(|m| m.len()).unwrap_or(0);
        let loads = font::Font::load(&p.to_string_lossy()).is_ok();
        println!(
            "  {:>8} KB  {}  {}",
            size / 1024,
            if loads { "ok  " } else { "FAIL" },
            p.display()
        );
    }
    Ok(())
}

/// Faces worth reaching for first. The Kindle ships its own reading fonts, and
/// Bookerly is the one Amazon designed for this panel — alphabetical order
/// would otherwise hand us an Arabic UI fallback.
const PREFERRED_FONTS: &[&str] = &[
    "Bookerly-Regular",
    "Amazon-Ember-Regular",
    "Baskerville-Regular",
    "Palatino-Regular",
];

fn pick_font() -> Option<String> {
    let found = find_fonts();
    for want in PREFERRED_FONTS {
        if let Some(p) = found.iter().find(|p| {
            p.file_stem()
                .and_then(|s| s.to_str())
                .is_some_and(|s| s == *want)
        }) {
            return Some(p.to_string_lossy().into_owned());
        }
    }
    found.first().map(|p| p.to_string_lossy().into_owned())
}

fn text_demo(font_path: Option<String>) -> std::io::Result<()> {
    let path = match font_path.or_else(pick_font) {
        Some(p) => p,
        None => {
            eprintln!("no font found — run `magkindle fonts` to see what is available");
            std::process::exit(1);
        },
    };
    println!("font: {path}");

    let f = font::Font::load(&path)?;
    let mut fb = Framebuffer::open()?;
    fb.clear(0xFF);

    let margin = 60u32;
    let width = fb.width - margin * 2;
    let mut y = margin;

    // A size ladder, so the panel's actual legibility is visible rather than
    // guessed at. 300 ppi flatters small type far more than a screen does.
    for px in [64.0f32, 44.0, 32.0, 24.0] {
        f.draw(
            &mut fb,
            margin,
            y + f.ascent(px),
            px,
            &format!("Magician {px:.0}px"),
            0x00,
        );
        y += f.line_height(px) + 6;
    }

    y += 20;
    let body = "The quick brown fox jumps over the lazy dog.                 Antialiasing is free on this panel: fontdue emits eight-bit                 coverage per pixel and the framebuffer is eight-bit greyscale,                 so glyph coverage blends straight in with no dithering and no                 conversion step.";
    let laid = f.draw_wrapped(&mut fb, margin, y, width, 28.0, body, 0x00);
    println!(
        "wrapped into {} lines, ending at y={}",
        laid.lines, laid.next_y
    );

    // GC16 full: the clean, flashing refresh. Text wants fidelity, not speed.
    fb.refresh(fb.full_rect(), Waveform::Gc16, UpdateMode::Full)?;
    println!("rendered — run `magkindle restore` to hand the screen back");
    Ok(())
}
