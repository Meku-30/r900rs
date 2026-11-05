// r900rs - Neptune R900 decoder in Rust
// Copyright (C) 2014 Douglas Hall (rtlamr)
// Copyright (C) 2026 Meku-30
// Derived from rtlamr (https://github.com/bemasher/rtlamr): protocol/decode.go.
// Modified 2026-11: ported from Go to Rust and restructured for lower CPU use;
// decoding results are kept identical to rtlamr v0.9.6.
// SPDX-License-Identifier: AGPL-3.0-or-later
//! 振幅・マンチェスターのフィルター・プリアンブルの探索 (rtlamr の protocol/decode.go)。
//! 浮動小数 (f32) の計算の順は rtlamr と同じ。足す順を変えない。
//! 量子化した列は 8 個ずつ詰めて持ち、プリアンブルの探索は 64 か所ずつビット演算で調べる (結果は rtlamr の 2 段の探索と同じ)。

pub const PREAMBLE: &str = "00000000000000001110010101100100";
pub const PREAMBLE_SYMBOLS: usize = 32;
pub const PACKET_SYMBOLS: usize = 116;

#[derive(Debug, Clone)]
pub struct Cfg {
    pub chip_length: usize,
    pub symbol_length: usize,
    pub preamble_length: usize,
    pub packet_length: usize,
    pub block_size: usize,
    pub buffer_length: usize,
}

impl Cfg {
    pub fn new(chip_length: usize) -> Cfg {
        let symbol_length = chip_length << 1;
        let preamble_length = PREAMBLE_SYMBOLS * symbol_length;
        let packet_length = PACKET_SYMBOLS * symbol_length;
        let block_size = preamble_length.next_power_of_two();
        Cfg {
            chip_length,
            symbol_length,
            preamble_length,
            packet_length,
            block_size,
            buffer_length: packet_length + block_size,
        }
    }
}

/// 振幅の表。lut[i] = ((127.5 - i) / 127.5)^2 (f32、rtlamr の NewMagLUT と同じ順の計算)
pub fn mag_lut() -> [f32; 256] {
    let mut lut = [0f32; 256];
    for (i, v) in lut.iter_mut().enumerate() {
        let x = (127.5f32 - i as f32) / 127.5f32;
        *v = x * x;
    }
    lut
}

pub struct Decoder {
    cfg: Cfg,
    lut: [f32; 256],
    signal: Vec<f32>,
    csum: Vec<f32>,
    /// rtlamr の Quantized (長さ buffer_length の 0/1) を 8 個ずつ詰めたもの (先頭が上位ビット)。
    /// rtlamr の packed (探索用) と同じ詰め方で、量子化した列そのものは持たない
    bits: Vec<u8>,
    /// フィルターの結果を詰める前の 0/1 (長さ block_size)
    qtmp: Vec<u8>,
    /// プリアンブルの各ビットの探索用の xor (1 なら 0、0 なら全部 1)
    pmask: [u64; PREAMBLE_SYMBOLS],
    idx: Vec<usize>,
}

impl Decoder {
    pub fn new(cfg: Cfg) -> Decoder {
        let signal = vec![0f32; cfg.block_size + cfg.symbol_length];
        let csum = vec![0f32; signal.len() + 1];
        // 探索は 8 個ずつ詰めた列を symbol_length / 8 バイトずつずらして読む (chip_length は 8 の倍数なので割り切れる)
        assert!(cfg.symbol_length % 8 == 0 && cfg.packet_length % 8 == 0 && cfg.block_size % 64 == 0);
        Decoder {
            lut: mag_lut(),
            signal,
            csum,
            bits: vec![0u8; cfg.buffer_length >> 3],
            qtmp: vec![0u8; cfg.block_size],
            pmask: std::array::from_fn(|i| if PREAMBLE.as_bytes()[i] == b'1' { 0 } else { !0 }),
            idx: Vec::with_capacity(cfg.block_size),
            cfg,
        }
    }

    /// 今のブロックの振幅 (長さ block_size + symbol_length)。
    /// 先頭の symbol_length 個は前のブロックの最後、後ろの block_size 個が新しい分
    pub fn signal(&self) -> &[f32] {
        &self.signal
    }

    #[allow(dead_code)]
    pub fn cfg(&self) -> &Cfg {
        &self.cfg
    }

    /// 1 ブロック (2 * block_size バイト) を入れて、プリアンブルの見つかった添字を返す (小さい順)
    pub fn decode(&mut self, block: &[u8]) -> &[usize] {
        let c = &self.cfg;
        assert_eq!(block.len(), 2 * c.block_size);
        // 前のブロックの最後の symbol_length 個を前に残し、量子化は block_size ずらす
        self.signal.copy_within(c.block_size.., 0);
        self.bits.copy_within(c.block_size >> 3.., 0);
        // 累積和は先頭から順に足す (足す順を変えない)。振幅の計算と同じループで
        let sl = c.symbol_length;
        let mut sum = 0f32;
        for i in 0..sl {
            sum += self.signal[i];
            self.csum[i + 1] = sum;
        }
        let lut = &self.lut;
        for ((o, cs), iq) in self.signal[sl..]
            .iter_mut()
            .zip(self.csum[sl + 1..].iter_mut())
            .zip(block.chunks_exact(2))
        {
            let v = lut[iq[0] as usize] + lut[iq[1] as usize];
            *o = v;
            sum += v;
            *cs = sum;
        }
        self.filter();
        self.search();
        &self.idx
    }

    /// rtlamr の Filter。結果 (0/1) を Quantized[packet_length..] の位置に詰めて書く
    fn filter(&mut self) {
        let c = &self.cfg;
        let out = &mut self.bits[c.packet_length >> 3..];
        let cs = &self.csum[..];
        let lower = &cs[c.chip_length..];
        let upper = &cs[c.symbol_length..];
        // 1 - 符号ビット (rtlamr と同じ。-0.0 は 0、+0.0 は 1) を 1 バイトずつ作ってから 8 個を 1 バイトに詰める
        let tmp = &mut self.qtmp;
        for (idx, t) in tmp.iter_mut().enumerate() {
            let l = lower[idx];
            let f = (l - cs[idx]) - (upper[idx] - l);
            *t = 1 - (f.to_bits() >> 31) as u8;
        }
        for (o, t) in out.iter_mut().zip(tmp.chunks_exact(8)) {
            // 0/1 の 8 バイト (先頭が下位) → 先頭を上位ビットにした 1 バイト
            let x = u64::from_le_bytes(t.try_into().unwrap());
            *o = (x.wrapping_mul(0x8040_2010_0804_0201) >> 56) as u8;
        }
    }

    /// rtlamr の Search と同じ結果 (Quantized[q + p * symbol_length] == preamble[p] が 32 個とも
    /// 成り立つ q (< block_size) の小さい順)。64 個の位置をまとめてビット演算で調べる
    fn search(&mut self) {
        let c = &self.cfg;
        let sym_byte = c.symbol_length >> 3;
        let bits = &self.bits[..];
        let word = |o: usize| u64::from_be_bytes(bits[o..o + 8].try_into().unwrap());
        self.idx.clear();
        for w in 0..c.block_size >> 6 {
            let base = w << 3;
            let mut acc = !0u64;
            // 1 なら v、0 なら !v (= v ^ !0)
            for (p_idx, &m) in self.pmask.iter().enumerate() {
                acc &= word(base + p_idx * sym_byte) ^ m;
                if acc == 0 {
                    break;
                }
            }
            while acc != 0 {
                let lz = acc.leading_zeros() as usize;
                self.idx.push((w << 6) + lz);
                acc &= !(1u64 << (63 - lz));
            }
        }
    }

    /// 量子化した列を 0/1 に戻す (テスト用)
    #[cfg(test)]
    fn quantized(&self) -> Vec<u8> {
        (0..self.cfg.buffer_length)
            .map(|i| (self.bits[i >> 3] >> (7 - (i & 7))) & 1)
            .collect()
    }
}

#[allow(dead_code)] // 今はテストだけで使う (読み取りは read_exact でブロックごと)
/// 届いたバイトをブロックの長さにまとめ、端数を次に持ち越す
pub struct BlockAssembler {
    block_len: usize,
    buf: Vec<u8>,
    /// 前の push で返した分 (次の push の頭で捨てる)
    emitted: usize,
}

#[allow(dead_code)]
impl BlockAssembler {
    pub fn new(block_len: usize) -> BlockAssembler {
        BlockAssembler {
            block_len,
            buf: Vec::new(),
            emitted: 0,
        }
    }

    pub fn push(&mut self, bytes: &[u8]) -> impl Iterator<Item = &[u8]> {
        self.buf.drain(..self.emitted);
        self.buf.extend_from_slice(bytes);
        self.emitted = self.buf.len() / self.block_len * self.block_len;
        self.buf[..self.emitted].chunks_exact(self.block_len)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lut_matches_go() {
        let lut = mag_lut();
        assert_eq!(lut[0], 1.0);
        assert_eq!(
            lut[128].to_bits(),
            ((127.5f32 - 128.0) / 127.5).powi(2).to_bits()
        );
    }

    #[test]
    fn cfg_values() {
        let c = Cfg::new(72);
        assert_eq!(c.symbol_length, 144);
        assert_eq!(c.preamble_length, 4608);
        assert_eq!(c.packet_length, 16704);
        assert_eq!(c.block_size, 8192);
        assert_eq!(c.buffer_length, 24896);
    }

    #[test]
    fn partial_reads_assemble_blocks() {
        let mut a = BlockAssembler::new(16384);
        let data: Vec<u8> = (0..40000u32).map(|i| (i % 251) as u8).collect();
        assert_eq!(a.push(&data[..3]).count(), 0);
        assert_eq!(a.push(&data[3..16383]).count(), 0);
        let got: Vec<Vec<u8>> = a.push(&data[16383..16384]).map(|b| b.to_vec()).collect();
        assert_eq!(got.len(), 1);
        assert_eq!(got[0], data[..16384]);
        // 端数の持ち越し: 2 つ目のブロックはちょうど 32768 で出る
        assert_eq!(a.push(&data[16384..32767]).count(), 0);
        let got: Vec<Vec<u8>> = a.push(&data[32767..32768]).map(|b| b.to_vec()).collect();
        assert_eq!(got.len(), 1);
        assert_eq!(got[0], data[16384..32768]);
        // 一度に 2 ブロック以上
        let big: Vec<u8> = vec![7; 16384 * 2 + 5];
        assert_eq!(a.push(&big).count(), 2);
        assert_eq!(a.push(&vec![7; 16384 - 5]).count(), 1);
    }

    /// 1 記号 = 2 チップのマンチェスター。1 は前のチップが強い
    fn build_block(cfg: &Cfg, start: usize, bits: &[u8]) -> Vec<u8> {
        let mut b = vec![128u8; 2 * cfg.block_size];
        for (n, &bit) in bits.iter().enumerate() {
            for c in 0..2 {
                let strong = (c == 0) == (bit == 1);
                if !strong {
                    continue;
                }
                for s in 0..cfg.chip_length {
                    let i = start + n * cfg.symbol_length + c * cfg.chip_length + s;
                    b[2 * i] = 255;
                    b[2 * i + 1] = 0;
                }
            }
        }
        b
    }

    fn run(cfg: &Cfg, bits: &[u8], start: usize, blocks: usize) -> Vec<Vec<usize>> {
        let mut d = Decoder::new(cfg.clone());
        let blank = vec![128u8; 2 * cfg.block_size];
        let mut out = Vec::new();
        out.push(d.decode(&build_block(cfg, start, bits)).to_vec());
        for _ in 1..blocks {
            out.push(d.decode(&blank).to_vec());
        }
        out
    }

    fn preamble_bits() -> Vec<u8> {
        PREAMBLE.bytes().map(|c| c - b'0').collect()
    }

    #[test]
    fn finds_synthetic_preamble() {
        for chip in [8usize, 72] {
            let cfg = Cfg::new(chip);
            let start = (cfg.block_size - cfg.preamble_length) / 2 + if chip == 72 { 5 } else { 0 };
            let bits = preamble_bits();
            // 振幅の位置 start は Signal の symbol_length + start、Quantized では packet_length を足す。
            // 探索の窓 (Quantized の先頭 block_size) に入るまで block_size ずつずれる
            let q0 = cfg.packet_length + cfg.symbol_length + start;
            let n = q0 / cfg.block_size + 1; // 見つかるブロックの数
            let want = q0 - (n - 1) * cfg.block_size;
            assert!(want < cfg.block_size);
            let r = run(&cfg, &bits, start, n);
            assert!(r[..n - 1].iter().all(|v| v.is_empty()), "chip {chip}: {:?}", r);
            assert!(r[n - 1].contains(&want), "chip {chip}: want {want} in {:?}", r[n - 1]);
            for k in 0..32 {
                let mut bad = bits.clone();
                bad[k] ^= 1;
                let r = run(&cfg, &bad, start, n);
                assert!(!r[n - 1].contains(&want), "chip {chip} bit {k}");
            }
        }
    }

    /// rtlamr の Search をそのまま写したもの (8 個ずつ詰めたバイトで粗く → 1 個ずつ)
    fn search_reference(cfg: &Cfg, quantized: &[u8]) -> Vec<usize> {
        let preamble = preamble_bits();
        let sym_len_byte = cfg.symbol_length >> 3;
        let packed: Vec<u8> = (0..(cfg.block_size + cfg.preamble_length + 7) >> 3)
            .map(|b| quantized[b << 3..(b + 1) << 3].iter().fold(0u8, |a, &q| (a << 1) | q))
            .collect();
        let mut idx: Vec<usize> = (0..cfg.block_size >> 3).collect();
        for (p_idx, &p_bit) in preamble.iter().enumerate() {
            let mask = (p_bit ^ 1) * 0xFF;
            idx.retain(|&q| packed[q + p_idx * sym_len_byte] != mask);
            if idx.is_empty() {
                return idx;
            }
        }
        let mut idx: Vec<usize> = idx.iter().flat_map(|&q| (0..8).map(move |i| (q << 3) + i)).collect();
        for (p_idx, &p_bit) in preamble.iter().enumerate() {
            let off = p_idx * cfg.symbol_length;
            idx.retain(|&q| quantized[off + q] == p_bit);
        }
        idx
    }

    /// ビット演算の探索が rtlamr の 2 段の探索と同じ添字を同じ順に返す
    #[test]
    fn bitwise_search_equals_reference() {
        for chip in [8usize, 72, 96] {
            let cfg = Cfg::new(chip);
            let mut d = Decoder::new(cfg.clone());
            let mut x: u64 = 0x9E37_79B9_7F4A_7C15 ^ chip as u64;
            let mut rnd = || {
                x ^= x << 13;
                x ^= x >> 7;
                x ^= x << 17;
                x
            };
            let pre = preamble_bits();
            let mut hits = 0;
            for round in 0..40 {
                // 量子化した列を直接作る: 乱数・長い連続・プリアンブルを何か所も (1 チップ前後のずれ込み)
                let mut q = vec![0u8; cfg.buffer_length];
                match round % 4 {
                    0 => q.iter_mut().for_each(|v| *v = (rnd() & 1) as u8),
                    1 => {
                        let mut cur = 0u8;
                        for v in q.iter_mut() {
                            if rnd() % 50 == 0 {
                                cur ^= 1;
                            }
                            *v = cur;
                        }
                    }
                    _ => {
                        q.iter_mut().for_each(|v| *v = (rnd() & 1) as u8);
                        for _ in 0..6 {
                            let at = (rnd() as usize) % cfg.block_size;
                            for (p, &b) in pre.iter().enumerate() {
                                for k in 0..cfg.symbol_length {
                                    q[at + p * cfg.symbol_length + k] = b;
                                }
                            }
                        }
                    }
                }
                for (i, &v) in q.iter().enumerate() {
                    let m = 0x80u8 >> (i & 7);
                    if v == 1 {
                        d.bits[i >> 3] |= m;
                    } else {
                        d.bits[i >> 3] &= !m;
                    }
                }
                assert_eq!(d.quantized(), q);
                d.search();
                let want = search_reference(&cfg, &q);
                assert_eq!(d.idx, want, "chip {chip} round {round}");
                hits += want.len();
            }
            assert!(hits > 0, "chip {chip}: 一致する所が 1 つもない (テストが弱い)");
            // ブロックを通した場合も同じ (振幅・累積和・フィルターのあと)
            for _ in 0..4 {
                let blk: Vec<u8> = (0..2 * cfg.block_size).map(|_| (rnd() >> 3) as u8).collect();
                let got = d.decode(&blk).to_vec();
                assert_eq!(got, search_reference(&cfg, &d.quantized()));
            }
        }
    }

    #[test]
    fn signal_has_block_plus_symbol_length() {
        let cfg = Cfg::new(8);
        let mut d = Decoder::new(cfg.clone());
        d.decode(&build_block(&cfg, 0, &preamble_bits()));
        assert_eq!(d.signal().len(), cfg.block_size + cfg.symbol_length);
    }
}
