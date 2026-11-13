//! `pdfcmp` — reference rasters and screenshot comparison for the viewer test
//! rig. Two verbs:
//!
//!   pdfcmp ref <file.pdf> <page> <width> <height> <out.png>
//!       Rasterise page `page` (1-based) at `width` px wide into a PNG. Height
//!       is capped: only the top `height` px of the page are rendered, because
//!       that is all the page area ever shows at zoom 1.
//!   pdfcmp cmp <ref.png> <shot.png> <area_h> [--save crop.png]
//!       Crop the top `area_h` px of the screenshot, align it to the reference
//!       and print one JSON line of numbers.
//!
//! It is built for the emulator (same NDK toolchain as the APK) so that the
//! references are produced next to the thing under test and no screenshot or
//! PDF has to travel back over the wire: the driver only reads text.

use std::path::Path;

use pathfinder_geometry::transform2d::Transform2F;
use pathfinder_geometry::vector::vec2f;
use pdf::file::FileOptions;
use pdf_render::vello_cpu_backend::{OutlineBuilder, VelloCpuBackend};
use pdf_render::{page_bounds, render_page, Cache};

fn die(msg: String) -> ! {
    println!("{msg}");
    std::process::exit(2)
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 2 {
        die("usage: pdfcmp ref <pdf> <page> <width> <height> <out.png> | pdfcmp cmp <ref.png> <shot.png> <area_h> [--save crop.png]".into());
    }
    match args[1].as_str() {
        "ref" => {
            if args.len() < 7 {
                die("usage: pdfcmp ref <pdf> <page> <width> <height> <out.png>".into());
            }
            let page: usize = args[3].parse().unwrap_or_else(|_| die("page must be a number".into()));
            let width: u32 = args[4].parse().unwrap_or_else(|_| die("width must be a number".into()));
            let height: u32 = args[5].parse().unwrap_or_else(|_| die("height must be a number".into()));
            render_ref(&args[2], page, width, height, &args[6]);
        }
        "cmp" => {
            if args.len() < 5 {
                die("usage: pdfcmp cmp <ref.png> <shot.png> <area_h> [--save crop.png]".into());
            }
            let area_h: u32 = args[4].parse().unwrap_or_else(|_| die("area_h must be a number".into()));
            let mut save: Option<String> = None;
            if let Some(i) = args.iter().position(|a| a == "--save") {
                save = args.get(i + 1).cloned();
            }
            compare(&args[2], &args[3], area_h, save.as_deref());
        }
        other => die(format!("unknown verb {other}")),
    }
}

fn render_ref(pdf: &str, page_nr: usize, width: u32, height: u32, out: &str) {
    if std::env::var_os("STANDARD_FONTS").is_none() {
        std::env::set_var("STANDARD_FONTS", std::env::temp_dir());
    }
    let file = match FileOptions::cached().open(Path::new(pdf)) {
        Ok(f) => f,
        Err(e) => die(format!("ref-failed open {e:?}")),
    };
    let page = match file.get_page(page_nr as u32 - 1) {
        Ok(p) => p,
        Err(e) => die(format!("ref-failed page {e:?}")),
    };
    let bounds = page_bounds(&page);
    let (mut pw, mut ph) = (bounds.width().max(1.0), bounds.height().max(1.0));
    if page.rotate() % 180 != 0 {
        std::mem::swap(&mut pw, &mut ph);
    }
    let scale = width as f32 / pw;
    let full_h = (ph * scale).ceil() as u32;
    let h = full_h.min(height.max(1)).min(6000) as u16;
    let mut cache = Cache::<OutlineBuilder>::new(OutlineBuilder::default());
    let resolver = file.resolver();
    let mut backend = VelloCpuBackend::new(&mut cache, width as u16, h);
    backend.fill_background(1.0, 1.0, 1.0);
    if let Err(e) = render_page(
        &mut backend,
        &resolver,
        &page,
        Transform2F::from_scale(vec2f(scale, scale)),
    ) {
        die(format!("ref-failed render {e:?}"));
    }
    let png = match backend.finish().into_png() {
        Ok(p) => p,
        Err(e) => die(format!("ref-failed png {e:?}")),
    };
    if let Err(e) = std::fs::write(out, png) {
        die(format!("ref-failed write {e:?}"));
    }
    println!("ref {}x{} full {}x{}", width, h, width, full_h);
}

struct Img {
    w: usize,
    h: usize,
    l: Vec<u8>,
}

fn luma_of(path: &str) -> Img {
    let img = match image::ImageReader::open(path) {
        Ok(r) => match r.decode() {
            Ok(i) => i,
            Err(e) => die(format!("cmp-failed decode {path}: {e}")),
        },
        Err(e) => die(format!("cmp-failed open {path}: {e}")),
    };
    let rgb = img.to_rgba8();
    let (w, h) = (rgb.width() as usize, rgb.height() as usize);
    let px = rgb.as_raw();
    let mut l = Vec::with_capacity(w * h);
    for i in 0..w * h {
        let r = px[i * 4] as u32;
        let g = px[i * 4 + 1] as u32;
        let b = px[i * 4 + 2] as u32;
        l.push(((299 * r + 587 * g + 114 * b) / 1000) as u8);
    }
    Img { w, h, l }
}

/// Mean absolute difference of a `bw x bh` window at the given offset, on
/// `step`-strided samples. Offset is ref -> shot: shot[x] ~ ref[x - dx].
fn mad_at(ref_: &Img, shot: &Img, dx: isize, dy: isize, h: usize, step: usize) -> f64 {
    let w = ref_.w.min(shot.w);
    let (mut sum, mut n) = (0u64, 0u64);
    let mut y = 0usize;
    while y < h {
        let sy = y as isize + dy;
        if sy < 0 || sy as usize >= shot.h {
            y += step;
            continue;
        }
        let mut x = 0usize;
        while x < w {
            let sx = x as isize + dx;
            if sx < 0 || sx as usize >= shot.w {
                x += step;
                continue;
            }
            let a = ref_.l[y * ref_.w + x] as i32;
            let b = shot.l[sy as usize * shot.w + sx as usize] as i32;
            sum += (a - b).unsigned_abs() as u64;
            n += 1;
            x += step;
        }
        y += step;
    }
    if n == 0 {
        return 255.0;
    }
    sum as f64 / n as f64
}

fn compare(ref_path: &str, shot_path: &str, area_h: u32, save: Option<&str>) {
    let ref_ = luma_of(ref_path);
    let shot = luma_of(shot_path);
    let h = (area_h as usize).min(ref_.h).min(shot.h);
    if h == 0 {
        die("cmp-failed: empty area".into());
    }
    let w = ref_.w.min(shot.w);

    // Align first. The page is not always at the top of the screen (a page
    // shorter than the area is centred), so the vertical offset is searched
    // over the whole screenshot: coarse at 1/16, then refined at full step.
    let max_dy = (shot.h as isize - h as isize) + 16;
    let mut best = (f64::MAX, 0isize, 0isize);
    let mut dy = -16isize;
    while dy <= max_dy {
        for dx in -6..=6isize {
            let m = mad_at(&ref_, &shot, dx, dy, h, 16);
            if m < best.0 {
                best = (m, dx, dy);
            }
        }
        dy += 4;
    }
    let (_, cdx, cdy) = best;
    best = (f64::MAX, cdx, cdy);
    for dy in (cdy - 8).max(-16)..=(cdy + 8).min(max_dy) {
        for dx in cdx - 4..=cdx + 4 {
            let m = mad_at(&ref_, &shot, dx, dy, h, 2);
            if m < best.0 {
                best = (m, dx, dy);
            }
        }
    }
    let (_, dx, dy) = best;

    // Full-resolution numbers at the best offset.
    let mut sum = 0u64;
    let mut n = 0u64;
    let mut bad = 0u64; // |delta| > 60: "not the same ink"
    let mut sum_ref = 0u64;
    for y in 0..h {
        let sy = y as isize + dy;
        if sy < 0 || sy as usize >= shot.h {
            continue;
        }
        for x in 0..w {
            let sx = x as isize + dx;
            if sx < 0 || sx as usize >= shot.w {
                continue;
            }
            let a = ref_.l[y * ref_.w + x] as i32;
            let b = shot.l[sy as usize * shot.w + sx as usize] as i32;
            let d = (a - b).unsigned_abs();
            sum += d as u64;
            if d > 60 {
                bad += 1;
            }
            sum_ref += a as u64;
            n += 1;
        }
    }
    let mad = if n == 0 { 255.0 } else { sum as f64 / n as f64 };
    let frac_bad = if n == 0 { 1.0 } else { bad as f64 / n as f64 };

    // 24x24 blocks: catches a tile that never got drawn (reference has ink,
    // screenshot is flat) and names where it happened.
    let (bs, mut flat, mut worst_blk, mut worst_at, mut blocks) = (24usize, 0usize, 0f64, (0usize, 0usize), 0usize);
    let mut by = 0usize;
    while by + bs <= h {
        let mut bx = 0usize;
        while bx + bs <= w {
            let (mut s, mut m, mut s2, mut m2, mut cnt) = (0f64, 0f64, 0f64, 0f64, 0u64);
            for y in by..by + bs {
                let sy = y as isize + dy;
                if sy < 0 || sy as usize >= shot.h {
                    continue;
                }
                for x in bx..bx + bs {
                    let sx = x as isize + dx;
                    if sx < 0 || sx as usize >= shot.w {
                        continue;
                    }
                    let a = ref_.l[y * ref_.w + x] as f64;
                    let b = shot.l[sy as usize * shot.w + sx as usize] as f64;
                    s += (a - b).abs();
                    m += a;
                    s2 += a * a;
                    m2 += b * b;
                    cnt += 1;
                }
            }
            if cnt > 0 {
                let c = cnt as f64;
                let mad_b = s / c;
                let var_ref = (s2 / c - (m / c) * (m / c)).max(0.0).sqrt();
                let mean_b = m2 / c;
                let _ = mean_b;
                blocks += 1;
                if mad_b > worst_blk {
                    worst_blk = mad_b;
                    worst_at = (bx, by);
                }
                // Ink in the reference, nothing at all in the screenshot.
                if var_ref > 18.0 && mad_b > 90.0 {
                    flat += 1;
                }
            }
            bx += bs;
        }
        by += bs;
    }

    let mut shot_mean = 0u64;
    let mut sc = 0u64;
    for y in 0..h {
        let sy = y as isize + dy;
        if sy < 0 || sy as usize >= shot.h {
            continue;
        }
        for x in 0..w {
            let sx = x as isize + dx;
            if sx < 0 || sx as usize >= shot.w {
                continue;
            }
            shot_mean += shot.l[sy as usize * shot.w + sx as usize] as u64;
            sc += 1;
        }
    }
    let shot_mean = if sc == 0 { 0.0 } else { shot_mean as f64 / sc as f64 };
    let ref_mean = if n == 0 { 0.0 } else { sum_ref as f64 / n as f64 };
    let black = shot_mean < 8.0 && ref_mean > 40.0;

    if let Some(p) = save {
        // The crop, for a human to look at when the numbers are bad.
        if let Some(img) = image::ImageReader::open(shot_path).ok().and_then(|r| r.decode().ok()) {
            let rgba = img.to_rgba8();
            let cw = w as u32;
            let ch = h as u32;
            let mut out = image::RgbaImage::new(cw, ch);
            for y in 0..ch {
                for x in 0..cw {
                    let sx = (x as isize + dx).clamp(0, rgba.width() as isize - 1) as u32;
                    let sy = (y as isize + dy).clamp(0, rgba.height() as isize - 1) as u32;
                    out.put_pixel(x, y, *rgba.get_pixel(sx, sy));
                }
            }
            let _ = out.save(p);
        }
    }

    println!(
        "{{\"mad\":{:.2},\"frac_bad\":{:.4},\"worst_blk\":{:.1},\"worst_at\":[{},{}],\"flat\":{},\"blocks\":{},\"dx\":{},\"dy\":{},\"shot_mean\":{:.1},\"ref_mean\":{:.1},\"black\":{},\"w\":{},\"h\":{}}}",
        mad, frac_bad, worst_blk, worst_at.0, worst_at.1, flat, blocks, dx, dy, shot_mean, ref_mean, black, w, h
    );
}
