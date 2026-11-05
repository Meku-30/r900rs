// r900rs - Neptune R900 decoder in Rust
// Copyright (C) 2014 Douglas Hall (rtlamr)
// Copyright (C) 2026 Meku-30
// Derived from rtlamr (https://github.com/bemasher/rtlamr): r900/r900.go, r900/gf/gf.go (encoder written as the inverse of the decoder).
// Modified 2026-11: ported from Go to Rust and restructured for lower CPU use;
// decoding results are kept identical to rtlamr v0.9.6.
// SPDX-License-Identifier: AGPL-3.0-or-later
//! テスト用の偽の R900 の電波 (rtlamr の r900.go の filter() と Syndrome から逆に組み立てる)。

#![allow(dead_code)]

use crate::decoder::PREAMBLE;
use crate::gf::Field;
use crate::r900::R900;

/// 欄を 80 ビットに並べ、RS のパリティ 5 記号を付けた 21 記号
pub fn encode(msg: &R900) -> [u8; 21] {
    let mut bits: u128 = 0;
    for (v, n) in [
        (msg.id as u128, 32),
        (msg.unkn1 as u128, 8),
        (msg.nouse as u128, 6),
        (msg.backflow as u128, 2),
        (msg.consumption as u128, 24),
        (msg.unkn3 as u128, 2),
        (msg.leak as u128, 4),
        (msg.leaknow as u128, 2),
    ] {
        bits = (bits << n) | (v & ((1u128 << n) - 1));
    }
    let mut out = [0u8; 21];
    for (i, o) in out[..16].iter_mut().enumerate() {
        *o = ((bits >> (75 - 5 * i)) & 31) as u8;
    }
    // 生成多項式 g(x) = ∏ (x - α^(29+i))、係数は次数の高い方から
    let f = Field::new();
    let mut g = vec![1u8];
    for i in 0..5 {
        let r = f.exp_of(29 + i);
        let mut ng = g.clone();
        ng.push(0);
        for j in 1..ng.len() {
            ng[j] ^= f.mul(r, g[j - 1]);
        }
        g = ng;
    }
    // 短縮符号: 記号 0..16 のあと 0 が 10 個、そのあとパリティ。余りを LFSR で
    let mut data = [0u8; 26];
    data[..16].copy_from_slice(&out[..16]);
    let mut rem = [0u8; 5];
    for &d in &data {
        let fb = d ^ rem[0];
        rem.copy_within(1.., 0);
        rem[4] = 0;
        for j in 0..5 {
            rem[j] ^= f.mul(fb, g[j + 1]);
        }
    }
    out[16..].copy_from_slice(&rem);
    out
}

/// 6 進の 1 桁の 4 チップ (true = 強い)
/// filter() の a0 = C0+C1-C2-C3、a1 = C0-C1+C2-C3、a2 = C0-C1-C2+C3 (Ck = k 番目のチップの和)。
/// 基本の形 (1100・1010・1001) の強いチップが正 → 桁 3〜5、反転 → 桁 0〜2
pub fn digit_chips(d: u8) -> [bool; 4] {
    const BASE: [[bool; 4]; 3] = [
        [true, true, false, false],
        [true, false, true, false],
        [true, false, false, true],
    ];
    let b = BASE[(d % 3) as usize];
    if d >= 3 {
        b
    } else {
        b.map(|c| !c)
    }
}

/// プリアンブル + 21 記号 (6 進 2 桁 = 4 チップ × 2) のチップの並び (232 個、true = 強い)
pub fn packet_chips(symbols: &[u8; 21]) -> Vec<bool> {
    let mut chips: Vec<bool> = Vec::with_capacity(232);
    // プリアンブル: 1 記号 = 2 チップのマンチェスター、1 は前のチップが強い
    for b in PREAMBLE.bytes() {
        chips.push(b == b'1');
        chips.push(b != b'1');
    }
    for &s in symbols {
        assert!(s < 32);
        chips.extend(digit_chips(s / 6));
        chips.extend(digit_chips(s % 6));
    }
    chips
}

/// プリアンブル + 21 記号の IQ (8 ビット、I Q の順)
pub fn packet_iq(symbols: &[u8; 21], chip: usize, amp: f32, offset_hz: f64, fs: f64) -> Vec<u8> {
    let chips = packet_chips(symbols);
    let mut out = Vec::with_capacity(chips.len() * chip * 2);
    let w = 2.0 * std::f64::consts::PI * offset_hz / fs;
    for (n, strong) in chips
        .iter()
        .flat_map(|&c| std::iter::repeat(c).take(chip))
        .enumerate()
    {
        let a = if strong { amp as f64 } else { 0.0 };
        let ph = w * n as f64;
        let i = (127.5 + 127.5 * a * ph.cos()).round().clamp(0.0, 255.0) as u8;
        let q = (127.5 + 127.5 * a * ph.sin()).round().clamp(0.0, 255.0) as u8;
        out.push(i);
        out.push(q);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gf::Field;

    fn rs_buf(s: &[u8; 21]) -> [u8; 31] {
        let mut b = [0u8; 31];
        b[..16].copy_from_slice(&s[..16]);
        b[26..].copy_from_slice(&s[16..]);
        b
    }

    #[test]
    fn syndrome_zero_for_encoded() {
        let f = Field::new();
        let msg = R900 {
            id: 12_345_678,
            consumption: 1_234_567,
            unkn1: 0x5A,
            leak: 7,
            ..Default::default()
        };
        let s = encode(&msg);
        assert!(s.iter().all(|&v| v < 32));
        assert_eq!(f.syndrome(&rs_buf(&s), 5, 29), [0; 5]);
        for i in 0..21 {
            let mut bad = s;
            bad[i] ^= 1;
            assert_ne!(f.syndrome(&rs_buf(&bad), 5, 29), [0; 5], "記号 {i}");
        }
        // 先頭 32 ビットが ID
        let bits: u128 = s.iter().fold(0u128, |a, &v| (a << 5) | v as u128);
        assert_eq!((bits >> (105 - 32)) as u32, 12_345_678);
    }

    #[test]
    fn packet_iq_length() {
        let s = [0u8; 21];
        let iq = packet_iq(&s, 72, 1.0, 0.0, 72.0 * 32_768.0);
        assert_eq!(iq.len(), 2 * 116 * 2 * 72);
    }
}
