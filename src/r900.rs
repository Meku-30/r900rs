// r900rs - Neptune R900 decoder in Rust
// Copyright (C) 2014 Douglas Hall (rtlamr)
// Copyright (C) 2026 Meku-30
// Derived from rtlamr (https://github.com/bemasher/rtlamr): r900/r900.go.
// Modified 2026-11: ported from Go to Rust and restructured for lower CPU use;
// decoding results are kept identical to rtlamr v0.9.6.
// SPDX-License-Identifier: AGPL-3.0-or-later
//! R900 の読み取り (rtlamr の r900/r900.go の filter と Parse)。
//! rtlamr は毎ブロック全部の位置で 4 チップのフィルターを計算するが、ここでは
//! 候補があるブロックだけ、使う 42 か所だけ計算する (結果は同じ)。

use crate::decoder::Cfg;
use crate::gf::Field;

pub const PAYLOAD_SYMBOLS: usize = 42;

/// Parser の振幅の窓を進める余り (ブロックの数)。この数のブロックごとに 1 回だけ前に戻す
const SPARE_BLOCKS: usize = 32;

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct R900 {
    pub id: u32,
    pub unkn1: u8,
    pub nouse: u8,
    pub backflow: u8,
    pub consumption: u32,
    pub unkn3: u8,
    pub leak: u8,
    pub leaknow: u8,
    pub checksum: [u8; 5],
    pub q_idx: usize,
}

pub struct Parser {
    cfg: Cfg,
    field: Field,
    /// rtlamr の p.signal (長さ buffer_length) は buf[start..start + buffer_length]。
    /// 毎ブロック全部をずらす代わりに、長い buf の中で窓を進め、端に来たときだけ前に戻す
    buf: Vec<f32>,
    start: usize,
    /// 累積和 (長さ buffer_length + 1)。毎回先頭から必要な所まで計算し直す
    csum: Vec<f32>,
}

impl Parser {
    pub fn new(cfg: Cfg) -> Parser {
        Parser {
            field: Field::new(),
            buf: vec![0f32; cfg.buffer_length + SPARE_BLOCKS * cfg.block_size],
            start: 0,
            csum: vec![0f32; cfg.buffer_length + 1],
            cfg,
        }
    }

    /// 毎ブロック呼ぶ。block_signal は Decoder::signal() の後ろの block_size 個 (新しい分)。
    /// rtlamr の copy(p.signal, p.signal[BlockSize:]); copy(p.signal[PacketLength:], d.Signal[SymbolLength:])
    pub fn push_signal(&mut self, block_signal: &[f32]) {
        let c = &self.cfg;
        assert_eq!(block_signal.len(), c.block_size);
        let (bs, pl, bl) = (c.block_size, c.packet_length, c.buffer_length);
        if self.start + bs + bl <= self.buf.len() {
            self.start += bs;
        } else {
            // 残す packet_length 個を前に戻す (SPARE_BLOCKS ブロックに 1 回)
            self.buf.copy_within(self.start + bs..self.start + bl, 0);
            self.start = 0;
        }
        let s = self.start;
        self.buf[s + pl..s + bl].copy_from_slice(block_signal);
    }

    /// rtlamr の p.signal と同じ中身 (長さ buffer_length)
    #[cfg(test)]
    fn signal(&self) -> &[f32] {
        &self.buf[self.start..self.start + self.cfg.buffer_length]
    }

    #[cfg(test)]
    fn signal_mut(&mut self) -> &mut [f32] {
        let (s, n) = (self.start, self.cfg.buffer_length);
        &mut self.buf[s..s + n]
    }

    /// 探索の結果 (Decoder::decode の qIdx、小さい順) から電文を読む。rtlamr の Parse と同じ順・同じ結果
    pub fn parse(&mut self, pkts: &[usize]) -> Vec<R900> {
        let mut out = Vec::new();
        if !self.prepare(pkts) {
            return out;
        }
        // 同じブロックの中で出たビット列 (21 記号で同じこと)
        let mut seen: Vec<[u8; 21]> = Vec::new();
        for &q in pkts {
            if q > self.cfg.block_size {
                break;
            }
            let digits = self.payload_digits(q);
            let mut symbols = [0u8; 21];
            let mut bad = false;
            for (s, d) in symbols.iter_mut().zip(digits.chunks_exact(2)) {
                let v = d[0] * 6 + d[1];
                if v > 31 {
                    bad = true;
                    break;
                }
                *s = v;
            }
            if bad || seen.contains(&symbols) {
                continue;
            }
            seen.push(symbols);

            let mut rs = [0u8; 31];
            rs[..16].copy_from_slice(&symbols[..16]);
            rs[26..].copy_from_slice(&symbols[16..]);
            if self.field.syndrome(&rs, 5, 29) != [0u8; 5] {
                continue;
            }

            let bits: u128 = symbols.iter().fold(0u128, |a, &v| (a << 5) | v as u128);
            // ビット列の [a:b] (先頭が 0)
            let field = |a: u32, b: u32| ((bits >> (105 - b)) & ((1u128 << (b - a)) - 1)) as u32;
            let mut checksum = [0u8; 5];
            checksum.copy_from_slice(&symbols[16..]);
            out.push(R900 {
                id: field(0, 32),
                unkn1: field(32, 40) as u8,
                nouse: field(40, 46) as u8,
                backflow: field(46, 48) as u8,
                consumption: field(48, 72),
                unkn3: field(72, 74) as u8,
                leak: field(74, 78) as u8,
                leaknow: field(78, 80) as u8,
                checksum,
                q_idx: q,
            });
        }
        out
    }

    /// 候補 pkts のうち使うもの (qIdx <= block_size) について、累積和を先頭から必要な所まで計算する。
    /// 使うものがなければ false
    fn prepare(&mut self, pkts: &[usize]) -> bool {
        let c = &self.cfg;
        let cl4 = 4 * c.chip_length;
        // rtlamr は qIdx > block_size で break する (小さい順なので、そこまでの最大)
        let Some(max_q) = pkts.iter().take_while(|&&q| q <= c.block_size).max() else {
            return false;
        };
        let last = max_q + c.preamble_length - c.symbol_length + (PAYLOAD_SYMBOLS - 1) * cl4;
        let upto = (last + cl4).min(c.buffer_length);
        // 先頭から順に足す (rtlamr の filter() と同じ順。途中で止めても同じ値)
        let mut sum = 0f32;
        let start = self.start;
        for (v, o) in self.buf[start..start + upto].iter().zip(self.csum[1..=upto].iter_mut()) {
            sum += *v;
            *o = sum;
        }
        true
    }

    /// 位置 idx の 4 チップの判定 (rtlamr の filter() の 1 か所分)。
    /// rtlamr は limit 以上の位置を書かない (0 のまま) ので 0
    fn quantize_at(&self, idx: usize) -> u8 {
        let c = &self.cfg;
        let cl = c.chip_length;
        let (cl2, cl3, cl4) = (cl * 2, cl * 3, cl * 4);
        if idx >= c.buffer_length - cl4 {
            return 0;
        }
        let cs = &self.csum;
        let c0 = cs[idx];
        let c1 = cs[idx + cl] + cs[idx + cl];
        let c2 = cs[idx + cl2] + cs[idx + cl2];
        let c3 = cs[idx + cl3] + cs[idx + cl3];
        let c4 = cs[idx + cl4];

        let a0 = c2 - c4 - c0; // 1100
        let a1 = c1 - c2 + c3 - c4 - c0; // 1010
        let a2 = c1 - c3 + c4 - c0; // 1001

        // 絶対値の大きい方。同じなら前のもの
        let mut max_abs = a0.abs();
        let mut argmax = 0u8;
        let mut sel = a0;
        if a1.abs() > max_abs {
            max_abs = a1.abs();
            argmax = 1;
            sel = a1;
        }
        if a2.abs() > max_abs {
            argmax = 2;
            sel = a2;
        }
        if sel > 0.0 {
            argmax + 3
        } else {
            argmax
        }
    }

    /// qIdx の電文の 42 桁 (prepare のあとに呼ぶ)
    fn payload_digits(&self, q_idx: usize) -> [u8; PAYLOAD_SYMBOLS] {
        let c = &self.cfg;
        let payload_idx = q_idx + c.preamble_length - c.symbol_length;
        let step = 4 * c.chip_length;
        std::array::from_fn(|k| self.quantize_at(payload_idx + k * step))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::decoder::Decoder;
    use crate::synth;

    fn sample() -> R900 {
        R900 {
            id: 12_345_678,
            unkn1: 0x5A,
            nouse: 0b101101,
            backflow: 2,
            consumption: 1_234_567,
            unkn3: 1,
            leak: 9,
            leaknow: 3,
            ..Default::default()
        }
    }

    /// 偽の電波を Decoder と Parser に通し、ブロックごとの結果を返す
    fn run_stream(chip: usize, iq: &[u8], extra_blocks: usize) -> Vec<Vec<R900>> {
        let cfg = Cfg::new(chip);
        let mut d = Decoder::new(cfg.clone());
        let mut p = Parser::new(cfg.clone());
        let blen = 2 * cfg.block_size;
        let mut stream = iq.to_vec();
        let total = (stream.len() + blen - 1) / blen + extra_blocks;
        stream.resize(total * blen, 128);
        let mut out = Vec::new();
        for blk in stream.chunks_exact(blen) {
            let pkts = d.decode(blk).to_vec();
            p.push_signal(&d.signal()[cfg.symbol_length..]);
            out.push(p.parse(&pkts));
        }
        out
    }

    /// 探索の窓の真ん中あたりに来るように、先頭に空白を置いた IQ
    fn placed_iq(chip: usize, symbols: &[u8; 21], amp: f32, offset_hz: f64) -> Vec<u8> {
        let cfg = Cfg::new(chip);
        let bs = cfg.block_size;
        let lead = (bs / 2 + bs - (cfg.packet_length + cfg.symbol_length) % bs) % bs;
        let fs = 32_768.0 * chip as f64;
        let mut iq = vec![128u8; 2 * lead];
        iq.extend(synth::packet_iq(symbols, chip, amp, offset_hz, fs));
        iq
    }

    #[test]
    fn digit_patterns_quantize() {
        // 6 つの 4 チップの形をそれぞれ単独で判定させる (符号の決まりの確認)
        for chip in [8usize, 72] {
            let cfg = Cfg::new(chip);
            for d in 0..6u8 {
                let mut p = Parser::new(cfg.clone());
                let chips = synth::digit_chips(d);
                let at = 100;
                for (k, &strong) in chips.iter().enumerate() {
                    for s in 0..chip {
                        p.signal_mut()[at + k * chip + s] = if strong { 2.0 } else { 0.0 };
                    }
                }
                // csum を全部計算するため qIdx = block_size の候補を使う
                assert!(p.prepare(&[cfg.block_size]));
                assert_eq!(p.quantize_at(at), d, "chip {chip} digit {d}");
            }
        }
    }

    #[test]
    fn parse_decodes_synthetic() {
        let msg = sample();
        let syms = synth::encode(&msg);
        for (chip, amp, off) in [(72usize, 1.0f32, 0.0f64), (8, 1.0, 0.0), (72, 0.4, 12_000.0)] {
            let iq = placed_iq(chip, &syms, amp, off);
            let r = run_stream(chip, &iq, 4);
            let all: Vec<&R900> = r.iter().flatten().collect();
            assert_eq!(all.len(), 1, "chip {chip} amp {amp}: {:?}", r);
            let got = all[0];
            let mut want = msg.clone();
            want.checksum.copy_from_slice(&syms[16..]);
            want.q_idx = got.q_idx;
            assert_eq!(*got, want);
            assert!(got.q_idx <= Cfg::new(chip).block_size);
        }
    }

    #[test]
    fn corrupted_symbol_rejected() {
        let mut syms = synth::encode(&sample());
        syms[3] ^= 0b00100;
        let iq = placed_iq(72, &syms, 1.0, 0.0);
        let r = run_stream(72, &iq, 4);
        assert!(r.iter().all(|v| v.is_empty()), "{:?}", r);
    }

    #[test]
    fn same_bits_twice_in_block_once() {
        let chip = 72;
        let cfg = Cfg::new(chip);
        let syms = synth::encode(&sample());
        let iq = placed_iq(chip, &syms, 1.0, 0.0);
        let mut d = Decoder::new(cfg.clone());
        let mut p = Parser::new(cfg.clone());
        let blen = 2 * cfg.block_size;
        let mut stream = iq.clone();
        stream.resize(((stream.len() + blen - 1) / blen + 4) * blen, 128);
        for blk in stream.chunks_exact(blen) {
            let pkts = d.decode(blk).to_vec();
            p.push_signal(&d.signal()[cfg.symbol_length..]);
            let r = p.parse(&pkts);
            if r.is_empty() {
                continue;
            }
            let q = r[0].q_idx;
            // q と q+1 は単独でもそれぞれ読める (= 重複を消すことの確認になる)
            assert_eq!(p.parse(&[q]).len(), 1);
            assert_eq!(p.parse(&[q + 1]).len(), 1);
            let both = p.parse(&[q, q + 1]);
            assert_eq!(both.len(), 1);
            assert_eq!(both[0].q_idx, q);
            // block_size を超える候補は使わない
            assert!(p.parse(&[cfg.block_size + 1]).is_empty());
            return;
        }
        panic!("見つからなかった");
    }

    /// rtlamr と同じく全部の位置で累積和と判定を計算する素直な版
    fn full_quantized(cfg: &Cfg, signal: &[f32]) -> Vec<u8> {
        let mut csum = vec![0f32; cfg.buffer_length + 1];
        let mut sum = 0f32;
        for (i, v) in signal.iter().enumerate() {
            sum += *v;
            csum[i + 1] = sum;
        }
        let cl = cfg.chip_length;
        let (cl2, cl3, cl4) = (cl * 2, cl * 3, cl * 4);
        let mut q = vec![0u8; cfg.buffer_length];
        for idx in 0..cfg.buffer_length - cl4 {
            let c0 = csum[idx];
            let c1 = csum[idx + cl] + csum[idx + cl];
            let c2 = csum[idx + cl2] + csum[idx + cl2];
            let c3 = csum[idx + cl3] + csum[idx + cl3];
            let c4 = csum[idx + cl4];
            let a = [c2 - c4 - c0, c1 - c2 + c3 - c4 - c0, c1 - c3 + c4 - c0];
            let mut max_abs = a[0].abs();
            let mut am = 0usize;
            if a[1].abs() > max_abs {
                max_abs = a[1].abs();
                am = 1;
            }
            if a[2].abs() > max_abs {
                am = 2;
            }
            q[idx] = am as u8 + if a[am] > 0.0 { 3 } else { 0 };
        }
        q
    }

    #[test]
    fn lazy_equals_full() {
        for chip in [8usize, 72] {
            let cfg = Cfg::new(chip);
            let mut p = Parser::new(cfg.clone());
            let mut x: u32 = 0x1234_5678;
            let mut rnd = || {
                x = x.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                (x >> 8) as f32 / (1u32 << 24) as f32 * 2.0
            };
            let bs = cfg.block_size;
            // 遠い候補・近い候補だけ・遠い候補 (前のブロックの累積和が残っていても使わないこと)
            let cands: [Vec<usize>; 4] = [
                vec![0, 1, bs / 2, bs - 1, bs],
                vec![0, 3],
                vec![bs / 3, bs],
                vec![2, bs - 7, bs],
            ];
            for c in cands.iter() {
                let blk: Vec<f32> = (0..bs).map(|_| rnd()).collect();
                p.push_signal(&blk);
                let full = full_quantized(&cfg, p.signal());
                assert!(p.prepare(c));
                let mut n = 0;
                for &q in c {
                    let got = p.payload_digits(q);
                    let base = q + cfg.preamble_length - cfg.symbol_length;
                    for k in 0..PAYLOAD_SYMBOLS {
                        assert_eq!(got[k], full[base + k * 4 * chip], "chip {chip} q {q} k {k}");
                        n += 1;
                    }
                }
                assert_eq!(n, c.len() * PAYLOAD_SYMBOLS);
                // parse もそのまま通る (乱数では何も出ないはず)
                let _ = p.parse(c);
            }
        }
    }

    /// 窓を進める・前に戻すを何回くり返しても、rtlamr と同じ (全部ずらして末尾に足した) 中身
    #[test]
    fn signal_window_matches_shifting() {
        let cfg = Cfg::new(8);
        let mut p = Parser::new(cfg.clone());
        let mut want = vec![0f32; cfg.buffer_length];
        for n in 0..(3 * SPARE_BLOCKS + 5) {
            let blk: Vec<f32> = (0..cfg.block_size).map(|i| (n * 100_000 + i) as f32).collect();
            want.copy_within(cfg.block_size.., 0);
            want[cfg.packet_length..].copy_from_slice(&blk);
            p.push_signal(&blk);
            assert_eq!(p.signal(), &want[..], "block {n}");
        }
    }

    #[test]
    fn no_candidates_does_nothing() {
        let cfg = Cfg::new(8);
        let mut p = Parser::new(cfg.clone());
        p.push_signal(&vec![1.0; cfg.block_size]);
        assert!(!p.prepare(&[cfg.block_size + 1]));
        assert!(p.parse(&[]).is_empty());
        assert!(p.parse(&[cfg.block_size + 1, cfg.block_size + 5]).is_empty());
    }
}
