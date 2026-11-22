//! Demo DSD generator: synthesizes the same arpeggio piece as the PCM demo tracks directly at
//! the DSD rate, 2nd-order sigma-delta modulates it to 1-bit, and writes DSF or DSDIFF.
//! usage: dsdgen <out.dsf|out.dff> <dsd_multiple 64|128|256|512> <base_hz> <notes_per_sec> <secs>
use std::f64::consts::PI;
use std::fs::File;
use std::io::{BufWriter, Write};

fn piece(t: f64, f: f64, nps: f64, secs: f64, right: bool) -> f64 {
    let n = (t * nps).floor();
    let tau = t - n / nps;
    let bar = ((t * nps / 8.0).floor() as i64).rem_euclid(4);
    let root = [0.0, -3.0, 5.0, 7.0][bar as usize];
    let m = (n as i64).rem_euclid(8) as usize;
    let semi = [0.0, 4.0, 7.0, 12.0, 16.0, 12.0, 7.0, 4.0][m];
    let env = (tau * 300.0).min(1.0) * (-4.0 * tau).exp();
    let fade = (t * 2.0).min(1.0) * ((secs - t) * 1.5).min(1.0).max(0.0);
    let note = env * (2.0 * PI * f * 2f64.powf((root + semi) / 12.0) * tau).sin();
    let bass = 0.12 * (2.0 * PI * f / 2.0 * 2f64.powf(root / 12.0) * t).sin();
    fade * ((if right { 0.22 } else { 0.30 }) * note + bass)
}

/// 2nd-order sigma-delta → bits (true = +1). Input kept ≤ 0.5 FS for loop stability.
struct Sdm {
    i1: f64,
    i2: f64,
    y: f64,
}
impl Sdm {
    fn step(&mut self, x: f64) -> bool {
        self.i1 += x - self.y;
        self.i2 += self.i1 - self.y;
        let bit = self.i2 >= 0.0;
        self.y = if bit { 1.0 } else { -1.0 };
        bit
    }
}

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let (out, mult, f, nps, secs) = (
        &a[1],
        a[2].parse::<u64>().unwrap(),
        a[3].parse::<f64>().unwrap(),
        a[4].parse::<f64>().unwrap(),
        a[5].parse::<f64>().unwrap(),
    );
    let fs = 44_100 * mult;
    let n = (fs as f64 * secs) as u64; // samples per channel
    let n_bytes = n.div_ceil(8) as usize;
    // Bytes per channel; bit order decided at write time.
    let mut ch: [Vec<u8>; 2] = [vec![0u8; n_bytes], vec![0u8; n_bytes]];
    for (c, buf) in ch.iter_mut().enumerate() {
        let mut sdm = Sdm { i1: 0.0, i2: 0.0, y: 0.0 };
        for i in 0..n {
            let t = i as f64 / fs as f64;
            if sdm.step(piece(t, f, nps, secs, c == 1)) {
                buf[(i / 8) as usize] |= 1 << (i % 8); // LSB-first (DSF); flipped for DFF
            }
        }
    }
    let mut w = BufWriter::new(File::create(out).unwrap());
    if out.ends_with(".dsf") {
        const BLOCK: usize = 4096;
        let blocks = n_bytes.div_ceil(BLOCK);
        let data_len = (blocks * BLOCK * 2) as u64;
        let total = 28 + 52 + 12 + data_len;
        w.write_all(b"DSD ").unwrap();
        w.write_all(&28u64.to_le_bytes()).unwrap();
        w.write_all(&total.to_le_bytes()).unwrap();
        w.write_all(&0u64.to_le_bytes()).unwrap(); // metadata pointer (mutagen adds ID3)
        w.write_all(b"fmt ").unwrap();
        w.write_all(&52u64.to_le_bytes()).unwrap();
        for v in [1u32, 0, 2, 2, fs as u32, 1] {
            w.write_all(&v.to_le_bytes()).unwrap(); // version, raw, stereo, 2ch, fs, 1 bit
        }
        w.write_all(&n.to_le_bytes()).unwrap();
        w.write_all(&(BLOCK as u32).to_le_bytes()).unwrap();
        w.write_all(&0u32.to_le_bytes()).unwrap();
        w.write_all(b"data").unwrap();
        w.write_all(&(12 + data_len).to_le_bytes()).unwrap();
        for b in 0..blocks {
            for c in &ch {
                let s = b * BLOCK;
                let e = (s + BLOCK).min(n_bytes);
                w.write_all(&c[s..e]).unwrap();
                w.write_all(&vec![0u8; BLOCK - (e - s)]).unwrap();
            }
        }
    } else {
        // DSDIFF: big-endian chunks, MSB-first bits, byte-interleaved channels.
        let data_len = (n_bytes * 2) as u64;
        let prop: Vec<u8> = {
            let mut p = Vec::new();
            p.extend_from_slice(b"SND ");
            p.extend_from_slice(b"FS  ");
            p.extend_from_slice(&4u64.to_be_bytes());
            p.extend_from_slice(&(fs as u32).to_be_bytes());
            p.extend_from_slice(b"CHNL");
            p.extend_from_slice(&10u64.to_be_bytes());
            p.extend_from_slice(&2u16.to_be_bytes());
            p.extend_from_slice(b"SLFTSRGT");
            p.extend_from_slice(b"CMPR");
            p.extend_from_slice(&20u64.to_be_bytes());
            p.extend_from_slice(b"DSD ");
            p.push(14);
            p.extend_from_slice(b"not compressed");
            p.push(0); // pad to even
            p
        };
        let fver = 4u64;
        let body = 4 + (12 + fver) + (12 + prop.len() as u64) + (12 + data_len);
        w.write_all(b"FRM8").unwrap();
        w.write_all(&body.to_be_bytes()).unwrap();
        w.write_all(b"DSD ").unwrap();
        w.write_all(b"FVER").unwrap();
        w.write_all(&fver.to_be_bytes()).unwrap();
        w.write_all(&0x0105_0000u32.to_be_bytes()).unwrap();
        w.write_all(b"PROP").unwrap();
        w.write_all(&(prop.len() as u64).to_be_bytes()).unwrap();
        w.write_all(&prop).unwrap();
        w.write_all(b"DSD ").unwrap();
        w.write_all(&data_len.to_be_bytes()).unwrap();
        let mut inter = Vec::with_capacity(n_bytes * 2);
        for i in 0..n_bytes {
            inter.push(ch[0][i].reverse_bits());
            inter.push(ch[1][i].reverse_bits());
        }
        w.write_all(&inter).unwrap();
    }
    w.flush().unwrap();
    eprintln!("{out}: DSD{mult} {fs} Hz, {n} samples/ch, {secs}s");
}
