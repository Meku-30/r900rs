// r900rs - Neptune R900 decoder in Rust
// Copyright (C) 2014 Douglas Hall (rtlamr)
// Copyright (C) 2026 Meku-30
// Derived from rtlamr (https://github.com/bemasher/rtlamr): r900/gf/gf.go.
// Modified 2026-11: ported from Go to Rust and restructured for lower CPU use;
// decoding results are kept identical to rtlamr v0.9.6.
// SPDX-License-Identifier: AGPL-3.0-or-later
//! GF(2^5) の表と Syndrome (rtlamr の r900/gf/gf.go)。

pub struct Field {
    /// 元の数 - 1 (= 31)。表の長さと指数の法
    pub order: usize,
    /// log[0] は使わない (order を入れる)
    pub log: [u8; 32],
    /// exp[i] = exp[i + 31] = α^i
    pub exp: [u8; 62],
}

impl Field {
    /// order 32、多項式 37 (x^5 + x^2 + 1)、生成元 2
    pub fn new() -> Field {
        const SIZE: usize = 32;
        const POLY: usize = 37;
        const ALPHA: usize = 2;
        let order = SIZE - 1;
        let mut f = Field {
            order,
            log: [0; 32],
            exp: [0; 62],
        };
        let mut x = 1usize;
        for i in 0..order {
            assert!(!(x == 1 && i != 0), "gf: 生成元が原始元でない");
            f.exp[i] = x as u8;
            f.exp[i + order] = x as u8;
            f.log[x] = i as u8;
            x = poly_mul(x, ALPHA, SIZE, POLY);
        }
        f.log[0] = order as u8;
        f
    }

    /// α^e (rtlamr の Exp。e は 0 以上)
    pub fn exp_of(&self, e: usize) -> u8 {
        self.exp[e % self.order]
    }

    pub fn mul(&self, x: u8, y: u8) -> u8 {
        if x == 0 || y == 0 {
            return 0;
        }
        self.exp[self.log[x as usize] as usize + self.log[y as usize] as usize]
    }

    /// rtlamr の Syndrome。parity 個 (5 以下) を返し、残りは 0
    pub fn syndrome(&self, msg: &[u8; 31], parity: usize, offset: usize) -> [u8; 5] {
        assert!(offset <= self.order && parity <= 5);
        let mut out = [0u8; 5];
        for (idx, o) in out.iter_mut().take(parity).enumerate() {
            let a = self.exp_of(offset + idx);
            let mut syn = msg[0];
            for &v in &msg[1..] {
                syn = self.mul(syn, a) ^ v;
            }
            *o = syn;
        }
        out
    }
}

/// rtlamr の mul (多項式の掛け算を poly で割った余り)
fn poly_mul(mut x: usize, mut y: usize, size: usize, poly: usize) -> usize {
    let mut z = 0;
    while x > 0 {
        if x & 1 != 0 {
            z ^= y;
        }
        x >>= 1;
        y <<= 1;
        if y & size != 0 {
            y ^= poly;
        }
    }
    z
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gf_tables() {
        let f = Field::new();
        assert_eq!(f.exp[0], 1);
        assert_eq!(f.exp[1], 2);
        // x^5 = x^2 + 1 (37 = 0b100101)
        assert_eq!(f.exp[5], 5);
        assert_eq!(f.exp[31], 1);
        for i in 0..31 {
            assert_eq!(f.log[f.exp[i] as usize] as usize, i);
            assert_eq!(f.exp[i + 31], f.exp[i]);
        }
        assert_eq!(f.log[0], 31);
    }
}
