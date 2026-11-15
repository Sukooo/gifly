//! Turning downloaded files into a thumbnail texture or a (shrunk) animation.

use std::{
    io::Cursor,
    sync::{Mutex, MutexGuard},
};

use eframe::egui::{ColorImage, TextureHandle, TextureOptions};
use image::{AnimationDecoder, ImageDecoder, ImageError, ImageFormat, RgbaImage, codecs::webp::WebPDecoder, imageops};

use super::{
    Ctx, Fail, Job, Stop, THUMB_PX,
    cache::{thumb_path, touch},
    fetch::media_bytes,
    lock,
};

/// Frames kept per animation, and the most decoded to pick them from.
const MAX_FRAMES: usize = 72;
const MAX_DECODE: usize = 360;
/// The most memory one animation's kept frames take. A very tall GIF keeps fewer frames, so it
/// can't fill the texture budget alone; a 300×300 one keeps all of `MAX_FRAMES` well under this.
const MAX_ANIM_BYTES: usize = 32 << 20;
/// The grid's widest tile shape, height to width: frames are kept tall enough to cover it.
const MIN_SHAPE: f32 = 0.4;
/// GIFs that would only shrink by a little are kept as they are: shrinking every frame costs more
/// than the few percent of memory it saves. Most GIFs are 300 wide, a common tile size.
const KEEP_SIZE: f32 = 0.85;
/// Largest animation canvas decoded: 4096×4096, 64 MB per full-size frame buffer. Only blocks
/// absurd or malicious files; bigger GIFs still get a still thumbnail.
const MAX_PIXELS: u64 = 4096 * 4096;
/// Canvases above this (4 MB per frame) decode one at a time, so a page of big GIFs can't stack
/// up several hundred MB across the workers.
const BIG_PIXELS: u64 = 1024 * 1024;
static BIG_DECODE: Mutex<()> = Mutex::new(());

pub struct Anim {
    frames: Vec<TextureHandle>,
    delays: Vec<u64>,
    total: u64,
    bytes: usize,
}

impl Anim {
    /// Memory its frames take.
    pub fn bytes(&self) -> usize {
        self.bytes
    }

    /// The frame to show at `ms`, and how long until the next one.
    pub fn frame_at(&self, ms: u64) -> (&TextureHandle, u64) {
        let mut t = ms % self.total;
        for (f, &d) in self.frames.iter().zip(&self.delays) {
            if t < d {
                return (f, d - t);
            }
            t -= d;
        }
        (&self.frames[0], 16)
    }
}

fn texture(cx: &Ctx, name: String, img: &RgbaImage) -> Result<TextureHandle, Fail> {
    let size = [img.width() as usize, img.height() as usize];
    check_texture_size(size, cx.egui.input(|i| i.max_texture_side))?;
    let ci = ColorImage::from_rgba_unmultiplied(size, img.as_raw());
    Ok(cx.egui.load_texture(name, ci, TextureOptions::LINEAR))
}

fn check_texture_size(size: [usize; 2], max: usize) -> Result<(), Fail> {
    if size.into_iter().any(|side| side > max) { Err(Fail::TooBig) } else { Ok(()) }
}

pub(super) fn load_thumb(cx: &Ctx, job: &Job) -> Result<TextureHandle, Stop> {
    let path = thumb_path(&cx.dir, &job.url);
    let img = match image::open(&path) {
        Ok(img) => {
            touch(&path);
            crate::stats::add(&crate::stats::CACHE_HITS, 1);
            img.to_rgba8()
        }
        Err(_) => thumb_image(cx, job)?,
    };
    texture(cx, format!("t{}", job.id), &img).map_err(Into::into)
}

/// Makes the thumbnail from the (downloaded) file and caches it on disk.
pub(super) fn thumb_image(cx: &Ctx, job: &Job) -> Result<RgbaImage, Stop> {
    let bytes = media_bytes(cx, job)?;
    let reader = || image::ImageReader::new(Cursor::new(&bytes)).with_guessed_format().map_err(|_| Fail::NotImage);
    // A big picture decodes alone, like a big animation, so a page of photos can't stack up
    // gigabytes across the workers.
    let (w, h) = reader()?.into_dimensions().map_err(undecodable)?;
    let _turn = (u64::from(w) * u64::from(h) > BIG_PIXELS).then(|| lock(&BIG_DECODE));
    let mut r = reader()?;
    let mut limits = image::Limits::default();
    // Room for a MAX_PIXELS canvas and its first frame.
    limits.max_alloc = Some(256 << 20);
    r.limits(limits);
    let img = r.decode().map_err(undecodable)?.thumbnail(THUMB_PX, THUMB_PX).to_rgba8();
    let _ = img.save_with_format(thumb_path(&cx.dir, &job.url), ImageFormat::Qoi);
    Ok(img)
}

fn undecodable(e: ImageError) -> Fail {
    if matches!(e, ImageError::Limits(_)) { Fail::TooBig } else { Fail::NotImage }
}

pub(super) fn load_anim(cx: &Ctx, job: &Job) -> Result<Anim, Stop> {
    let small = anim_frames(media_bytes(cx, job)?, job.px)?;
    let bytes = small.iter().map(|(img, _)| img.as_raw().len()).sum();
    let (images, delays): (Vec<_>, Vec<_>) = small.into_iter().unzip();
    let frames = (images.into_iter().enumerate())
        .map(|(i, img)| texture(cx, format!("a{}_{i}", job.id), &img))
        .collect::<Result<_, _>>()?;
    Ok(Anim { total: delays.iter().sum(), frames, delays, bytes })
}

/// An animation's frames, shrunk to fit a tile `px` wide and thinned out, each with how long it
/// shows.
fn anim_frames(bytes: Vec<u8>, px: u32) -> Result<Vec<(RgbaImage, u64)>, Fail> {
    let small = match image::guess_format(&bytes).map_err(|_| Fail::NotImage)? {
        ImageFormat::Gif => gif_anim(&bytes, px)?,
        ImageFormat::WebP => webp_anim(bytes, px)?,
        _ => return Err(Fail::Still),
    };
    if small.len() < 2 {
        return Err(Fail::Still);
    }
    Ok(thin(small))
}

/// Keeps every n-th frame of a long or very tall animation, holding it for the skipped ones, so
/// every visible tile can play without blowing up texture memory.
fn thin(mut frames: Vec<(RgbaImage, u64)>) -> Vec<(RgbaImage, u64)> {
    let frame_bytes = frames[0].0.as_raw().len().max(1);
    let keep = MAX_FRAMES.min(MAX_ANIM_BYTES / frame_bytes).max(2);
    let step = frames.len().div_ceil(keep);
    if step > 1 {
        frames = frames.chunks_mut(step).map(|c| (std::mem::take(&mut c[0].0), c.iter().map(|f| f.1).sum())).collect();
    }
    frames
}

/// Frames are decoded at full size before they're shrunk, so canvas size is what costs memory.
/// Waits its turn for a big canvas.
fn canvas_turn(w: u32, h: u32) -> Result<Option<MutexGuard<'static, ()>>, Fail> {
    let pixels = u64::from(w) * u64::from(h);
    if pixels > MAX_PIXELS {
        return Err(Fail::TooBig);
    }
    Ok((pixels > BIG_PIXELS).then(|| lock(&BIG_DECODE)))
}

/// Browsers treat near-zero delays as 100ms; match that.
fn delay(ms: u64) -> u64 {
    if ms <= 10 { 100 } else { ms }
}

/// A GIF's frames, drawn onto one canvas that's copied (shrunk) only for the frames kept. Every
/// frame is still decoded, since each is drawn over the last.
fn gif_anim(bytes: &[u8], px: u32) -> Result<Vec<(RgbaImage, u64)>, Fail> {
    let step = gif_frames(bytes).map_or(1, |n| n.div_ceil(MAX_FRAMES).max(1));
    let mut options = gif::DecodeOptions::new();
    options.set_color_output(gif::ColorOutput::Indexed);
    let mut decoder = options.read_info(bytes).map_err(|_| Fail::NotImage)?;
    let (w, h) = (u32::from(decoder.width()), u32::from(decoder.height()));
    let global = decoder.global_palette().unwrap_or_default().to_vec();
    let _turn = canvas_turn(w, h)?;
    let size = fit(w, h, px);
    let mut canvas = RgbaImage::new(w, h);
    let (mut patch, mut saved) = (Vec::new(), Vec::new());
    let mut small: Vec<(RgbaImage, u64)> = Vec::new();
    for i in 0..MAX_DECODE {
        let Ok(Some(f)) = decoder.next_frame_info() else { break };
        let (left, top, fw, fh) = (u32::from(f.left), u32::from(f.top), u32::from(f.width), u32::from(f.height));
        // A frame can be much larger than the logical canvas. Check before allocating its patch.
        if u64::from(fw) * u64::from(fh) > MAX_PIXELS {
            return Err(Fail::TooBig);
        }
        let (dispose, ms) = (f.dispose, delay(u64::from(f.delay) * 10));
        // Each palette number's colour; numbers past the palette's end show through, as does
        // the transparent one.
        let mut colours = [[0u8; 4]; 256];
        for (c, &[r, g, b]) in colours.iter_mut().zip(f.palette.as_deref().unwrap_or(&global).as_chunks::<3>().0) {
            *c = [r, g, b, 255];
        }
        if let Some(t) = f.transparent {
            colours[usize::from(t)][3] = 0;
        }
        patch.resize(decoder.buffer_size(), 0);
        if decoder.read_into_buffer(&mut patch).is_err() {
            break;
        }
        // The part of the canvas the frame covers, as byte ranges of each of its rows.
        let (x0, x1) = (left.min(w) as usize * 4, (left + fw).min(w) as usize * 4);
        let rows = top.min(h)..(top + fh).min(h);
        let row = |y: u32| (y * w) as usize * 4;
        if dispose == gif::DisposalMethod::Previous {
            saved.clear();
            for y in rows.clone() {
                saved.extend_from_slice(&(*canvas)[row(y) + x0..row(y) + x1]);
            }
        }
        for y in rows.clone() {
            let src = &patch[((y - top) * fw) as usize..][..(x1 - x0) / 4];
            let dst = &mut (*canvas)[row(y) + x0..row(y) + x1];
            // A see-through pixel leaves what's under it.
            for (pixel, &n) in dst.as_chunks_mut::<4>().0.iter_mut().zip(src) {
                let c = colours[usize::from(n)];
                if c[3] != 0 {
                    *pixel = c;
                }
            }
        }
        match small.last_mut() {
            Some(last) if i % step != 0 => last.1 += ms,
            _ => small.push((shrink(&canvas, size), ms)),
        }
        match dispose {
            gif::DisposalMethod::Background => {
                for y in rows {
                    (*canvas)[row(y) + x0..row(y) + x1].fill(0);
                }
            }
            gif::DisposalMethod::Previous => {
                for (k, y) in rows.enumerate() {
                    (*canvas)[row(y) + x0..row(y) + x1].copy_from_slice(&saved[k * (x1 - x0)..][..x1 - x0]);
                }
            }
            _ => {}
        }
    }
    Ok(small)
}

fn webp_anim(bytes: Vec<u8>, px: u32) -> Result<Vec<(RgbaImage, u64)>, Fail> {
    let d = WebPDecoder::new(Cursor::new(bytes)).map_err(undecodable)?;
    let (w, h) = d.dimensions();
    let _turn = canvas_turn(w, h)?;
    let size = fit(w, h, px);
    let mut small = Vec::new();
    for f in d.into_frames().take(MAX_DECODE) {
        let Ok(f) = f else { break };
        let (n, dn) = f.delay().numer_denom_ms();
        small.push((shrink(f.buffer(), size), delay(u64::from(n / dn.max(1)))));
    }
    Ok(small)
}

/// The size to keep a `w` by `h` animation's frames at: enough to cover a tile `px` wide and at
/// least `MIN_SHAPE` of that tall, but never bigger than the GIF itself, nor smaller only by a
/// little (`KEEP_SIZE`).
fn fit(w: u32, h: u32, px: u32) -> (u32, u32) {
    let px = px as f32;
    let scale = (px / w as f32).max(px * MIN_SHAPE / h as f32);
    let scale = if scale >= KEEP_SIZE { 1.0 } else { scale };
    let at = |n: u32| ((n as f32 * scale).round() as u32).max(1);
    (at(w), at(h))
}

fn shrink(buf: &RgbaImage, (w, h): (u32, u32)) -> RgbaImage {
    if buf.dimensions() == (w, h) { buf.clone() } else { imageops::thumbnail(buf, w, h) }
}

/// How many frames a GIF has (up to `MAX_DECODE`), read without decoding any.
fn gif_frames(bytes: &[u8]) -> Option<usize> {
    let mut o = gif::DecodeOptions::new();
    o.skip_frame_decoding(true);
    let mut d = o.read_info(bytes).ok()?;
    let mut n = 0;
    // Reading complete skipped frames still reserves a buffer from their declared size.
    // Metadata advances without that allocation, even for a malformed frame descriptor.
    while n < MAX_DECODE && d.next_frame_info().ok()?.is_some() {
        n += 1;
    }
    Some(n)
}

#[cfg(test)]
mod tests {
    use image::{Delay, Frame, Rgba, codecs::gif::GifEncoder};

    use super::*;

    #[test]
    fn rejects_a_huge_patch_on_a_small_canvas_before_decoding() {
        let mut bytes = gif(2, 1);
        let at = bytes.iter().position(|&b| b == 0x2c).unwrap();
        // Inflate only the first image descriptor, leaving the 1x1 logical canvas untouched.
        bytes[at + 5..at + 9].copy_from_slice(&[255; 4]);
        assert_eq!(gif_anim(&bytes, 1).unwrap_err(), Fail::TooBig);
    }

    #[test]
    fn rejects_textures_larger_than_the_renderers_limit() {
        assert_eq!(check_texture_size([1, 65_535], 16_384), Err(Fail::TooBig));
        assert_eq!(check_texture_size([16_385, 1], 16_384), Err(Fail::TooBig));
        assert_eq!(check_texture_size([300, 300], 16_384), Ok(()));
    }

    fn gif(frames: usize, size: u32) -> Vec<u8> {
        let mut out = Vec::new();
        let mut enc = GifEncoder::new(&mut out);
        for i in 0..frames {
            let img = RgbaImage::from_pixel(size, size, Rgba([i as u8, 0, 0, 255]));
            enc.encode_frame(Frame::from_parts(img, 0, 0, Delay::from_numer_denom_ms(40, 1))).unwrap();
        }
        drop(enc);
        out
    }

    #[test]
    fn long_animations_are_thinned_out_keeping_their_length() {
        let frames = anim_frames(gif(200, 300), 250).unwrap();
        assert_eq!(gif_frames(&gif(200, 300)), Some(200));
        // Every 3rd frame, each held for three.
        assert_eq!(frames.len(), 67);
        assert_eq!(frames.iter().map(|f| f.1).sum::<u64>(), 200 * 40);
        assert_eq!(frames[0].0.dimensions(), (250, 250));
        assert_eq!(frames[1].0.get_pixel(0, 0)[0], 3);
    }

    #[test]
    fn big_frames_are_thinned_out_to_fit_the_memory_cap() {
        // 1200×1200 frames are 5.5 MB each, so five fit in `MAX_ANIM_BYTES`.
        let frames = (0..10).map(|_| (RgbaImage::new(1200, 1200), 40)).collect();
        let kept = thin(frames);
        assert_eq!(kept.len(), 5);
        assert_eq!(kept.iter().map(|f| f.1).sum::<u64>(), 400);
    }

    /// Patches over part of the canvas, see-through pixels, a frame's own palette, and every
    /// way of clearing a frame: drawn just as the image crate draws them.
    #[test]
    fn frames_are_stacked_like_the_image_crate_does() {
        use std::borrow::Cow;

        use image::{AnimationDecoder, codecs::gif::GifDecoder};
        let mut out = Vec::new();
        let mut enc = gif::Encoder::new(&mut out, 8, 8, &[255, 0, 0, 0, 255, 0, 0, 0, 255, 255, 255, 255]).unwrap();
        let frame = |(left, top, size): (u16, u16, u16), px: &[u8], dispose, transparent, palette: Option<Vec<u8>>| {
            gif::Frame {
                left,
                top,
                width: size,
                height: size,
                buffer: Cow::Owned(px.repeat(usize::from(size * size) / px.len())),
                dispose,
                transparent,
                palette,
                delay: 4,
                ..Default::default()
            }
        };
        use gif::DisposalMethod::{Background, Keep, Previous};
        for f in [
            frame((0, 0, 8), &[0], Keep, None, None),
            frame((2, 2, 4), &[1, 2], Previous, Some(2), None),
            frame((4, 4, 4), &[3, 2], Background, Some(2), None),
            frame((0, 0, 2), &[1], Keep, None, Some(vec![9, 9, 9, 200, 200, 200])),
            frame((0, 0, 8), &[2], Keep, Some(2), None),
            // Off the canvas entirely: draws nothing.
            frame((9, 0, 2), &[1], Previous, None, None),
        ] {
            enc.write_frame(&f).unwrap();
        }
        drop(enc);
        let ours = gif_anim(&out, 8).unwrap();
        let theirs: Vec<_> = GifDecoder::new(Cursor::new(&out)).unwrap().into_frames().map(|f| f.unwrap()).collect();
        assert_eq!(ours.len(), 6);
        for ((img, ms), f) in ours.iter().zip(&theirs) {
            assert!(img == f.buffer());
            assert_eq!(*ms, 40);
        }
        // The see-through last frame shows what the clearing left: red, the corner patch
        // painted grey, the cleared patch's square gone.
        assert_eq!(ours[4].0.get_pixel(0, 0).0, [200, 200, 200, 255]);
        assert_eq!(ours[4].0.get_pixel(3, 3).0, [255, 0, 0, 255]);
        assert_eq!(ours[4].0.get_pixel(6, 6).0, [0, 0, 0, 0]);
    }

    #[test]
    fn small_short_animations_are_kept_whole() {
        let frames = anim_frames(gif(5, 100), 250).unwrap();
        assert_eq!(frames.len(), 5);
        assert_eq!(frames[4].0.dimensions(), (100, 100));
        assert_eq!(anim_frames(gif(1, 100), 250).unwrap_err(), Fail::Still);
    }

    #[test]
    fn frames_are_kept_as_wide_as_the_tile_and_no_bigger_than_the_gif() {
        assert_eq!(fit(498, 280, 293), (293, 165));
        // Tall: as wide as the tile; wide: tall enough for the tile's shortest shape.
        assert_eq!(fit(400, 800, 250), (250, 500));
        assert_eq!(fit(1000, 200, 250), (500, 100));
        assert_eq!(fit(200, 150, 293), (200, 150));
        assert_eq!(fit(300, 225, 293), (300, 225));
        assert_eq!(fit(350, 225, 293), (293, 188));
    }
}
