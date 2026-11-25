//! Generated covers for albums and artists without art: a soft multi-stop gradient with a
//! film-grain texture, **deterministic** from the album / artist name, the same name gives the
//! same cover on every launch, machine and platform (desktop, DAP, a future mobile client).
//!
//! - **Seed:** FNV-1a 64 (a fixed, specified hash, never `std`'s randomized `DefaultHasher`)
//!   of the *normalized* name (Unicode NFC, lower-cased, whitespace trimmed + collapsed), so
//!   `"Björk"` in NFC or NFD, `"björk "` and `"BJÖRK"` share a cover. Albums are keyed by title
//!   (the library groups albums by title, so every track of a compilation shares one cover);
//!   artists by name. Prefixes keep an album and an artist with the same name apart.
//! - **Picture:** two or three hues picked from the seed in Oklch (perceptually even; no muddy
//!   browns, no neon), a linear gradient at a seeded angle plus a soft seeded highlight blob,
//!   interpolated in Oklab (no grey dead zones), then luminance grain from a hash of
//!   `(seed, x, y)` on a fixed 512-unit grid, stateless, so any size or render order agrees.
//! - **Same bits everywhere:** transcendental maths goes through `libm` (pure Rust), not the
//!   platform's C library, so macOS, Windows and Linux render byte-identical covers.
//! - **Versioned:** [`ALGO_VERSION`] is mixed into every seed and pinned by golden digests in
//!   the tests; change the look → bump it, and on-disk caches keyed by seed invalidate.
#![forbid(unsafe_code)]

use unicode_normalization::UnicodeNormalization;

/// Bump whenever the rendered look changes (the golden-digest tests enforce it).
pub const ALGO_VERSION: u32 = 1;

/// Normalized identity text: NFC, lower-case, whitespace trimmed and collapsed.
pub fn normalize(s: &str) -> String {
    let nfc: String = s.nfc().collect();
    nfc.to_lowercase()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn fnv1a64(parts: &[&[u8]]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for part in parts {
        for &b in *part {
            h ^= u64::from(b);
            h = h.wrapping_mul(0x0000_0100_0000_01b3);
        }
    }
    h
}

fn seed_for(kind: &[u8], name: &str) -> Option<u64> {
    let n = normalize(name);
    (!n.is_empty()).then(|| {
        fnv1a64(&[
            kind,
            b"\x1f",
            &ALGO_VERSION.to_le_bytes(),
            b"\x1f",
            n.as_bytes(),
        ])
    })
}

/// The seed for an album titled `album` (`None` for an empty title).
pub fn album_seed(album: &str) -> Option<u64> {
    seed_for(b"album", album)
}

/// The seed for an artist named `artist` (`None` for an empty name).
pub fn artist_seed(artist: &str) -> Option<u64> {
    seed_for(b"artist", artist)
}

/// SplitMix64: a tiny, fully specified generator for the palette / layout parameters.
struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }
    /// Uniform in `[lo, hi)`.
    fn range(&mut self, lo: f32, hi: f32) -> f32 {
        lo + (hi - lo) * ((self.next() >> 40) as f32 / (1u64 << 24) as f32)
    }
}

/// Oklab colour (L, a, b).
type Lab = [f32; 3];

fn oklch(l: f32, c: f32, h_deg: f32) -> Lab {
    let h_deg = h_deg.rem_euclid(360.0);
    // Yellows/olives only read as clean colour when light: lift them out of the mud.
    let yellowish = (55.0..=120.0).contains(&h_deg);
    let l = if yellowish { l.max(0.80) } else { l };
    let h = h_deg.to_radians();
    [l, c * libm::cosf(h), c * libm::sinf(h)]
}

/// A base hue in `[0, 360)` that skips the muddy olive band (60°–115°).
fn base_hue(u: f32) -> f32 {
    const SKIP: (f32, f32) = (60.0, 115.0);
    let h = u * (360.0 - (SKIP.1 - SKIP.0));
    if h < SKIP.0 { h } else { h + (SKIP.1 - SKIP.0) }
}

fn lerp(a: Lab, b: Lab, t: f32) -> Lab {
    [
        a[0] + (b[0] - a[0]) * t,
        a[1] + (b[1] - a[1]) * t,
        a[2] + (b[2] - a[2]) * t,
    ]
}

/// Oklab → gamma-encoded sRGB in `[0, 1]` (Björn Ottosson's reference matrices).
fn to_srgb([l, a, b]: Lab) -> [f32; 3] {
    let l_ = l + 0.396_337_78 * a + 0.215_803_76 * b;
    let m_ = l - 0.105_561_346 * a - 0.063_854_17 * b;
    let s_ = l - 0.089_484_18 * a - 1.291_485_5 * b;
    let (l3, m3, s3) = (l_ * l_ * l_, m_ * m_ * m_, s_ * s_ * s_);
    let lin = [
        4.076_741_7 * l3 - 3.307_711_6 * m3 + 0.230_969_94 * s3,
        -1.268_438 * l3 + 2.609_757_4 * m3 - 0.341_319_38 * s3,
        -0.004_196_086_3 * l3 - 0.703_418_6 * m3 + 1.707_614_7 * s3,
    ];
    lin.map(|c| {
        let c = c.clamp(0.0, 1.0);
        if c <= 0.003_130_8 {
            12.92 * c
        } else {
            1.055 * libm::powf(c, 1.0 / 2.4) - 0.055
        }
    })
}

/// The cover's layout parameters, all derived from the seed.
struct Design {
    stops: [Lab; 3],
    dir: [f32; 2],
    blob: [f32; 2],
    blob_r: f32,
}

fn design(seed: u64) -> Design {
    let mut r = Rng(seed);
    let hue = base_hue(r.range(0.0, 1.0));
    // Analogous most of the time, a split-complementary accent now and then.
    let step = if r.range(0.0, 1.0) < 0.25 {
        r.range(140.0, 200.0)
    } else {
        r.range(28.0, 70.0)
    } * if r.range(0.0, 1.0) < 0.5 { -1.0 } else { 1.0 };
    let c = r.range(0.09, 0.15);
    let stops = [
        oklch(r.range(0.42, 0.55), c, hue),
        oklch(r.range(0.62, 0.74), c * r.range(0.8, 1.1), hue + step),
        oklch(
            r.range(0.70, 0.82),
            c * r.range(0.6, 1.0),
            hue + step * 0.5 + r.range(-15.0, 15.0),
        ),
    ];
    let angle = r.range(0.0, std::f32::consts::TAU);
    Design {
        stops,
        dir: [libm::cosf(angle), libm::sinf(angle)],
        blob: [r.range(0.15, 0.85), r.range(0.15, 0.85)],
        blob_r: r.range(0.30, 0.55),
    }
}

/// SplitMix64's finalizer: a strong 64-bit mixer, so neighbouring cells decorrelate.
fn mix(mut z: u64) -> u64 {
    z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    z ^ (z >> 31)
}

/// Stateless per-cell grain in `[-1, 1]`.
fn grain(seed: u64, gx: u32, gy: u32) -> f32 {
    let h = mix(seed ^ mix((u64::from(gx) << 32) | u64::from(gy)));
    ((h >> 40) as f32 / (1u64 << 24) as f32) * 2.0 - 1.0
}

/// Grain amplitude (luminance, sRGB units), subtle texture, never noise.
const GRAIN: f32 = 0.045;

/// Render the cover for `seed` as `px × px` RGBA8 (opaque).
pub fn render_rgba(seed: u64, px: u32) -> Vec<u8> {
    let d = design(seed);
    let px = px.max(1);
    let mut out = Vec::with_capacity((px * px * 4) as usize);
    let inv = 1.0 / px as f32;
    for y in 0..px {
        for x in 0..px {
            let (u, v) = ((x as f32 + 0.5) * inv, (y as f32 + 0.5) * inv);
            let t = ((u - 0.5) * d.dir[0] + (v - 0.5) * d.dir[1]) * 1.15 + 0.5;
            let t = t.clamp(0.0, 1.0);
            let t = t * t * (3.0 - 2.0 * t); // smoothstep
            let mut lab = lerp(d.stops[0], d.stops[1], t);
            let (dx, dy) = (u - d.blob[0], v - d.blob[1]);
            let w = libm::expf(-(dx * dx + dy * dy) / (d.blob_r * d.blob_r)) * 0.65;
            lab = lerp(lab, d.stops[2], w);
            let rgb = to_srgb(lab);
            // Grain on a fixed 512-unit grid, so every size shows the same texture.
            let n = grain(seed, x * 512 / px, y * 512 / px) * GRAIN;
            for c in rgb {
                out.push(((c + n).clamp(0.0, 1.0) * 255.0).round() as u8);
            }
            out.push(255);
        }
    }
    out
}

/// Render the cover for `seed` as a `px × px` PNG.
pub fn render_png(seed: u64, px: u32) -> Vec<u8> {
    let px = px.max(1);
    let rgba = render_rgba(seed, px);
    let mut buf = Vec::new();
    {
        let mut enc = png::Encoder::new(&mut buf, px, px);
        enc.set_color(png::ColorType::Rgba);
        enc.set_depth(png::BitDepth::Eight);
        let mut w = enc.write_header().expect("in-memory PNG header");
        w.write_image_data(&rgba).expect("in-memory PNG data");
    }
    buf
}

#[cfg(test)]
mod tests {
    use super::*;

    fn digest(bytes: &[u8]) -> u64 {
        fnv1a64(&[bytes])
    }

    #[test]
    fn spellings_of_the_same_name_share_a_seed() {
        let nfc = "Bj\u{f6}rk";
        let nfd = "Bjo\u{308}rk";
        assert_ne!(nfc, nfd);
        let s = artist_seed(nfc);
        for v in [nfd, "  björk ", "BJÖRK", "Bj\u{f6}rk"] {
            assert_eq!(artist_seed(v), s, "{v:?}");
        }
        assert_eq!(album_seed("Kind  of   Blue"), album_seed("kind of blue"));
    }

    #[test]
    fn albums_and_artists_with_the_same_name_differ_and_empty_names_have_no_seed() {
        assert_ne!(album_seed("Weezer"), artist_seed("Weezer"));
        assert_eq!(album_seed("   "), None);
        assert_eq!(artist_seed(""), None);
    }

    #[test]
    fn rendering_is_deterministic_and_opaque() {
        let seed = album_seed("Hi-Res Ladder").unwrap();
        let a = render_rgba(seed, 64);
        assert_eq!(a, render_rgba(seed, 64), "same seed, same pixels");
        assert_eq!(a.len(), 64 * 64 * 4);
        assert!(a.chunks(4).all(|p| p[3] == 255));
        assert_ne!(a, render_rgba(album_seed("Lossy Formats").unwrap(), 64));
    }

    /// Golden digests pin the look across platforms and versions. If this fails because the
    /// algorithm changed on purpose, bump [`ALGO_VERSION`] and update the digests.
    #[test]
    fn golden_digests_pin_the_rendered_look() {
        let got: Vec<u64> = ["Kind of Blue", "Hi-Res Ladder", "Unknown Pleasures"]
            .iter()
            .map(|n| digest(&render_rgba(album_seed(n).unwrap(), 48)))
            .collect();
        assert_eq!(
            got, GOLDEN,
            "rendering changed: bump ALGO_VERSION and update GOLDEN"
        );
    }
    const GOLDEN: [u64; 3] = [
        13443793531659844743,
        6594088857587219185,
        17937714983630889307,
    ];

    #[test]
    fn different_names_get_visibly_different_palettes() {
        // Mean colour of a small render; distinct names should rarely land close together.
        let mean = |seed: u64| {
            let px = render_rgba(seed, 16);
            let mut m = [0f32; 3];
            for p in px.chunks(4) {
                for c in 0..3 {
                    m[c] += f32::from(p[c]);
                }
            }
            m.map(|v| v / 256.0)
        };
        let names: Vec<String> = (0..200).map(|i| format!("Album number {i}")).collect();
        let means: Vec<[f32; 3]> = names.iter().map(|n| mean(album_seed(n).unwrap())).collect();
        let mut close = 0;
        for i in 0..means.len() {
            for j in i + 1..means.len() {
                let d: f32 = (0..3)
                    .map(|c| (means[i][c] - means[j][c]).powi(2))
                    .sum::<f32>()
                    .sqrt();
                if d < 6.0 {
                    close += 1;
                }
            }
        }
        let pairs = means.len() * (means.len() - 1) / 2;
        assert!(
            close * 100 < pairs,
            "{close} of {pairs} pairs nearly identical"
        );
    }

    #[test]
    fn png_round_trips_at_the_requested_size() {
        let png_bytes = render_png(artist_seed("Soul Demo Ensemble").unwrap(), 96);
        let dec = png::Decoder::new(std::io::Cursor::new(png_bytes));
        let mut r = dec.read_info().unwrap();
        let mut buf = vec![0; r.output_buffer_size().unwrap()];
        let info = r.next_frame(&mut buf).unwrap();
        assert_eq!((info.width, info.height), (96, 96));
    }

    /// Visual check: `SOUL_COVER_SHEET=/tmp/sheet.png cargo test -p soul-player-host
    /// contact_sheet -- --ignored` writes a 4×3 grid of covers for real-looking names.
    #[test]
    #[ignore = "writes a PNG for eyeballing the look"]
    fn contact_sheet() {
        let Ok(out) = std::env::var("SOUL_COVER_SHEET") else {
            return;
        };
        let names = [
            "Kind of Blue",
            "Unknown Pleasures",
            "Hi-Res Ladder",
            "Lossless Formats",
            "Lossy Formats",
            "Direct Stream Digital",
            "Surround Fold-Down",
            "Selected Ambient Works",
            "Blue Train",
            "Homogenic",
            "Discovery",
            "In Rainbows",
        ];
        let (cell, gap, cols) = (256u32, 16u32, 4u32);
        let rows = (names.len() as u32).div_ceil(cols);
        let (w, h) = (
            cols * cell + (cols + 1) * gap,
            rows * cell + (rows + 1) * gap,
        );
        let mut sheet = vec![30u8; (w * h * 4) as usize];
        for (i, n) in names.iter().enumerate() {
            let px = render_rgba(album_seed(n).unwrap(), cell);
            let (cx, cy) = (
                gap + (i as u32 % cols) * (cell + gap),
                gap + (i as u32 / cols) * (cell + gap),
            );
            for y in 0..cell {
                let src = (y * cell * 4) as usize;
                let dst = (((cy + y) * w + cx) * 4) as usize;
                sheet[dst..dst + (cell * 4) as usize]
                    .copy_from_slice(&px[src..src + (cell * 4) as usize]);
            }
        }
        let mut buf = Vec::new();
        let mut enc = png::Encoder::new(&mut buf, w, h);
        enc.set_color(png::ColorType::Rgba);
        enc.set_depth(png::BitDepth::Eight);
        enc.write_header()
            .unwrap()
            .write_image_data(&sheet)
            .unwrap();
        std::fs::write(out, buf).unwrap();
    }
}
