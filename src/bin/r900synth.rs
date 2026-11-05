// r900rs - Neptune R900 decoder in Rust
// Copyright (C) 2026 Meku-30
// SPDX-License-Identifier: AGPL-3.0-or-later
//! r900synth: rtlamr と r900rs を比べるための偽の R900 の電波 (8 ビットの IQ、I Q の順) を作る。
//! r900synth -o <ファイル> -seconds=<秒> -symbollength=<n> -seed=<n> [-noise=<0..1>] [-offset=<Hz>] [-cw]
//!
//! - 20〜90 秒のランダムな間隔で、ランダムな ID・積算値の R900 を入れる。
//!   ID は 4,000,000,000 以上 (偽の値。本物のメーターの ID は使わない)
//! - noise: 雑音の強さ。電文の振幅は (1 - noise) × 0.6〜1.0、雑音の標準偏差は noise × 0.5。
//!   既定 0.1。1 なら電文なし (雑音だけ)
//! - offset: 電文の中心からのずれ (Hz) を全部この値にする (既定は電文ごとに ±30 kHz の乱数)
//! - cw: ファイル全体を一定値にする (強い連続波だけ。電文なし)

#[allow(dead_code)]
#[path = "../decoder.rs"]
mod decoder;
#[allow(dead_code)]
#[path = "../gf.rs"]
mod gf;
#[allow(dead_code)]
#[path = "../r900.rs"]
mod r900;
#[allow(dead_code)]
#[path = "../synth.rs"]
mod synth;

use r900::R900;
use std::io::{BufWriter, Write};

const DATA_RATE: f64 = 32_768.0;

struct Opts {
    out: String,
    seconds: f64,
    symbollength: usize,
    seed: u64,
    noise: f64,
    offset: Option<f64>,
    cw: bool,
}

fn parse() -> Result<Opts, String> {
    let mut o = Opts {
        out: String::new(),
        seconds: 600.0,
        symbollength: 72,
        seed: 1,
        noise: 0.1,
        offset: None,
        cw: false,
    };
    let mut it = std::env::args().skip(1);
    while let Some(a) = it.next() {
        if a == "-o" {
            o.out = it.next().ok_or("-o のあとにファイル名がない")?;
            continue;
        }
        if a == "-cw" {
            o.cw = true;
            continue;
        }
        let (k, v) = a
            .strip_prefix('-')
            .and_then(|b| b.split_once('='))
            .ok_or_else(|| format!("知らない指定: {a}"))?;
        let e = |x: &dyn std::fmt::Display| format!("{k}: {x}");
        match k {
            "seconds" => o.seconds = v.parse().map_err(|x| e(&x))?,
            "symbollength" => {
                o.symbollength = v.parse().map_err(|x| e(&x))?;
                if ![8, 32, 40, 48, 56, 64, 72, 80, 88, 96].contains(&o.symbollength) {
                    return Err(format!("symbollength は 8, 32, 40, 48, 56, 64, 72, 80, 88, 96: {v}"));
                }
            }
            "seed" => o.seed = v.parse().map_err(|x| e(&x))?,
            "offset" => o.offset = Some(v.parse().map_err(|x| e(&x))?),
            "noise" => o.noise = v.parse().map_err(|x| e(&x))?,
            _ => return Err(format!("知らない指定: {a}")),
        }
    }
    if o.out.is_empty() {
        return Err("-o <ファイル> が要る".into());
    }
    if !(0.0..=1.0).contains(&o.noise) {
        return Err("noise は 0〜1".into());
    }
    Ok(o)
}

/// splitmix64
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
    /// [0, 1)
    fn unif(&mut self) -> f64 {
        (self.next() >> 11) as f64 / (1u64 << 53) as f64
    }
    fn range(&mut self, lo: f64, hi: f64) -> f64 {
        lo + (hi - lo) * self.unif()
    }
    /// 平均 0・標準偏差 1 に近い値 (16 ビットの一様乱数 4 個の和、Irwin-Hall)
    fn gauss(&mut self) -> f64 {
        let r = self.next();
        let s: u64 = (0..4).map(|k| (r >> (16 * k)) & 0xFFFF).sum();
        (s as f64 / 65_536.0 - 2.0) * 3f64.sqrt()
    }
}

struct Packet {
    start: usize,
    chips: Vec<bool>,
    amp: f64,
    w: f64,
    phase: f64,
}

fn main() {
    let o = match parse() {
        Ok(o) => o,
        Err(e) => {
            eprintln!("r900synth: {e}");
            std::process::exit(2);
        }
    };
    if let Err(e) = run(&o) {
        eprintln!("r900synth: {e}");
        std::process::exit(1);
    }
}

fn run(o: &Opts) -> std::io::Result<()> {
    let chip = o.symbollength;
    let fs = DATA_RATE * chip as f64;
    let total = (o.seconds * fs) as usize;
    let mut rng = Rng(o.seed);
    let pkt_len = 232 * chip;

    // 電文の並び (始まりのサンプルの順)
    let mut pkts: Vec<Packet> = Vec::new();
    let sig_amp = 1.0 - o.noise;
    if !o.cw && sig_amp > 0.0 {
        let mut t = 0.0;
        loop {
            t += rng.range(20.0, 90.0);
            let start = (t * fs) as usize;
            if start + pkt_len + fs as usize > total {
                break;
            }
            let msg = R900 {
                id: 4_000_000_000 + (rng.next() % 200_000_000) as u32,
                unkn1: rng.next() as u8,
                nouse: (rng.next() & 0x3F) as u8,
                backflow: (rng.next() & 3) as u8,
                consumption: (rng.next() & 0xFF_FFFF) as u32,
                unkn3: (rng.next() & 3) as u8,
                leak: (rng.next() & 0xF) as u8,
                leaknow: (rng.next() & 3) as u8,
                ..Default::default()
            };
            pkts.push(Packet {
                start,
                chips: synth::packet_chips(&synth::encode(&msg)),
                amp: sig_amp * rng.range(0.6, 1.0),
                w: 2.0 * std::f64::consts::PI * o.offset.unwrap_or_else(|| rng.range(-30_000.0, 30_000.0)) / fs,
                phase: rng.range(0.0, 2.0 * std::f64::consts::PI),
            });
        }
    }

    let mut w = BufWriter::with_capacity(1 << 20, std::fs::File::create(&o.out)?);
    if o.cw {
        let buf = [200u8, 60].repeat(1 << 16);
        let mut left = total;
        while left > 0 {
            let n = left.min(1 << 16);
            w.write_all(&buf[..2 * n])?;
            left -= n;
        }
        w.flush()?;
        eprintln!("r900synth: 一定値 {total} サンプル");
        return Ok(());
    }

    let sigma = o.noise * 0.5;
    let q = |v: f64| (127.5 + 127.5 * v).round().clamp(0.0, 255.0) as u8;
    let mut next_pkt = 0;
    let mut n = 0usize;
    let mut buf: Vec<u8> = Vec::with_capacity(2 << 16);
    while n < total {
        let end = (n + (1 << 16)).min(total);
        buf.clear();
        for s in n..end {
            let (mut i, mut qv) = if sigma > 0.0 {
                (sigma * rng.gauss(), sigma * rng.gauss())
            } else {
                (0.0, 0.0)
            };
            while next_pkt < pkts.len() && s >= pkts[next_pkt].start + pkt_len {
                next_pkt += 1;
            }
            if let Some(p) = pkts.get(next_pkt) {
                if s >= p.start && p.chips[(s - p.start) / chip] {
                    let ph = p.phase + p.w * (s - p.start) as f64;
                    i += p.amp * ph.cos();
                    qv += p.amp * ph.sin();
                }
            }
            buf.push(q(i));
            buf.push(q(qv));
        }
        w.write_all(&buf)?;
        n = end;
    }
    w.flush()?;
    eprintln!("r900synth: {total} サンプル、電文 {} 件", pkts.len());
    Ok(())
}
