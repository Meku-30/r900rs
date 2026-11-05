// r900rs - Neptune R900 decoder in Rust
// Copyright (C) 2026 Meku-30
// SPDX-License-Identifier: AGPL-3.0-or-later
//! 電文の中心からのずれ (Hz)。受信の窓を置く場所を決めるための測定で、解読の結果には影響しない。

/// iq (8 ビットの I Q の順) の `start` 番目のサンプルから `len` 個の区間で
/// Σ x[n]·conj(x[n-1]) の角度 × fs / 2π を返す (x = (I-127.5) + j(Q-127.5))。
/// 区間が iq からはみ出すときは NaN
pub fn offset_hz(iq: &[u8], start: usize, len: usize, sample_rate: f64) -> f64 {
    if len < 2 || start.checked_add(len).map_or(true, |e| e * 2 > iq.len()) {
        return f64::NAN;
    }
    let (mut re, mut im) = (0f64, 0f64);
    let mut prev: Option<(f64, f64)> = None;
    for s in iq[2 * start..2 * (start + len)].chunks_exact(2) {
        let cur = (s[0] as f64 - 127.5, s[1] as f64 - 127.5);
        if let Some(p) = prev {
            // cur · conj(prev)
            re += cur.0 * p.0 + cur.1 * p.1;
            im += cur.1 * p.0 - cur.0 * p.1;
        }
        prev = Some(cur);
    }
    im.atan2(re) * sample_rate / (2.0 * std::f64::consts::PI)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::synth;

    #[test]
    fn offset_of_synthetic_tone() {
        let fs = 72.0 * 32_768.0;
        for hz in [150_000.0, -300_000.0, 0.0, 12_345.0] {
            let iq = synth::packet_iq(&[7u8; 21], 72, 0.8, hz, fs);
            let n = iq.len() / 2;
            let got = offset_hz(&iq, 0, n, fs);
            assert!((got - hz).abs() < 2_000.0, "want {hz} got {got}");
            // 前後に余白があっても区間だけを見る
            let mut padded = vec![128u8; 2 * 1000];
            padded.extend(&iq);
            padded.extend(vec![128u8; 2 * 1000]);
            let got = offset_hz(&padded, 1000, n, fs);
            assert!((got - hz).abs() < 2_000.0, "padded want {hz} got {got}");
        }
        assert!(offset_hz(&[128u8; 10], 3, 10, fs).is_nan());
    }
}
