// r900rs - Neptune R900 decoder in Rust
// Copyright (C) 2014 Douglas Hall (rtlamr)
// Copyright (C) 2026 Meku-30
// Derived from rtlamr (https://github.com/bemasher/rtlamr): main.go (output, duplicate suppression).
// Modified 2026-11: ported from Go to Rust and restructured for lower CPU use;
// decoding results are kept identical to rtlamr v0.9.6.
// SPDX-License-Identifier: AGPL-3.0-or-later
//! 出力 (JSON の 1 行) と、ブロックにまたがる重複の除き方 (rtlamr の main.go の prev / next)。

use crate::r900::R900;
use std::collections::HashSet;
use std::time::{SystemTime, UNIX_EPOCH};

/// 要約 = (Unkn1, ID, checksum)。種類は R900 だけなので省く
type Digest = (u8, u32, [u8; 5]);

/// rtlamr と同じ: `-filterid` で絞ってから要約を `next` に入れ、
/// 前のブロック (`prev`) にあったものは出さない。ブロックの終わりに入れ替える
pub struct Dedup {
    filterid: Vec<u32>,
    prev: HashSet<Digest>,
    next: HashSet<Digest>,
}

impl Dedup {
    pub fn new(filterid: Vec<u32>) -> Dedup {
        Dedup {
            filterid,
            prev: HashSet::new(),
            next: HashSet::new(),
        }
    }

    /// 1 ブロックぶんの電文を入れて、出してよいものを返す。ブロックごとに 1 回呼ぶ (空でも)
    pub fn filter(&mut self, msgs: Vec<R900>) -> Vec<R900> {
        self.next.clear();
        let mut out = Vec::new();
        for m in msgs {
            if !self.filterid.is_empty() && !self.filterid.contains(&m.id) {
                continue;
            }
            let d = (m.unkn1, m.id, m.checksum);
            self.next.insert(d);
            if self.prev.contains(&d) {
                continue;
            }
            out.push(m);
        }
        std::mem::swap(&mut self.prev, &mut self.next);
        out
    }
}

/// UNIX 時刻 (秒、ナノ秒) → "YYYY-MM-DDTHH:MM:SS.nnnnnnnnnZ" (UTC)
pub fn rfc3339_utc(secs: u64, nanos: u32) -> String {
    let days = (secs / 86_400) as i64;
    let rem = secs % 86_400;
    // days-from-civil の逆 (Howard Hinnant)
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + (m <= 2) as i64;
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}.{nanos:09}Z",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60
    )
}

pub fn now_rfc3339() -> String {
    let d = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default();
    rfc3339_utc(d.as_secs(), d.subsec_nanos())
}

/// 注意: Time は UTC・Offset/Length は常に 0 で、rtlamr と意味が違う (rtlamr はローカル時刻・ファイルの位置・バッファーの長さ)
/// rtlamr の -format=json と同じ欄の名前と順 (serde なしで手で組む)。`Time` は今の時刻
pub fn json_line(msg: &R900, freq_offset_hz: Option<f64>) -> String {
    json_line_at(&now_rfc3339(), msg, freq_offset_hz)
}

fn json_line_at(time: &str, msg: &R900, freq_offset_hz: Option<f64>) -> String {
    let mut s = format!(
        "{{\"Time\":\"{time}\",\"Offset\":0,\"Length\":0,\"Type\":\"R900\",\"Message\":{{\
\"ID\":{},\"Unkn1\":{},\"NoUse\":{},\"BackFlow\":{},\"Consumption\":{},\
\"Unkn3\":{},\"Leak\":{},\"LeakNow\":{}}}",
        msg.id, msg.unkn1, msg.nouse, msg.backflow, msg.consumption, msg.unkn3, msg.leak, msg.leaknow
    );
    if let Some(f) = freq_offset_hz.filter(|f| f.is_finite()) {
        s.push_str(&format!(",\"FreqOffsetHz\":{f}"));
    }
    s.push('}');
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    fn msg(id: u32, ck: u8) -> R900 {
        R900 { id, checksum: [ck; 5], ..Default::default() }
    }

    #[test]
    fn duplicate_in_next_block_suppressed() {
        let mut d = Dedup::new(vec![]);
        assert_eq!(d.filter(vec![msg(1, 7)]).len(), 1);
        // 次のブロックにも同じ要約 → 出さない
        assert_eq!(d.filter(vec![msg(1, 7)]).len(), 0);
        // 間に空のブロックが入ると prev から消える → 3 ブロック目はまた出る
        assert_eq!(d.filter(vec![]).len(), 0);
        assert_eq!(d.filter(vec![msg(1, 7)]).len(), 1);
        // checksum が違えば別の電文
        assert_eq!(d.filter(vec![msg(1, 8)]).len(), 1);
    }

    #[test]
    fn filterid_applied_before_dedup() {
        let mut d = Dedup::new(vec![1]);
        let out = d.filter(vec![msg(2, 7), msg(1, 7)]);
        assert_eq!(out.iter().map(|m| m.id).collect::<Vec<_>>(), vec![1]);
        // 除かれた ID は要約に入らない (prev にも残らない)
        assert!(d.prev.iter().all(|k| k.1 == 1));
        assert_eq!(d.filter(vec![msg(2, 7), msg(1, 7)]).len(), 0);
        assert_eq!(d.filter(vec![msg(2, 7)]).len(), 0);
    }

    #[test]
    fn json_shape() {
        let m = R900 {
            id: 12,
            unkn1: 1,
            nouse: 2,
            backflow: 3,
            consumption: 456,
            unkn3: 4,
            leak: 5,
            leaknow: 6,
            ..Default::default()
        };
        assert_eq!(
            json_line_at("2026-11-05T01:02:03.000000004Z", &m, None),
            "{\"Time\":\"2026-11-05T01:02:03.000000004Z\",\"Offset\":0,\"Length\":0,\"Type\":\"R900\",\"Message\":{\"ID\":12,\"Unkn1\":1,\"NoUse\":2,\"BackFlow\":3,\"Consumption\":456,\"Unkn3\":4,\"Leak\":5,\"LeakNow\":6}}"
        );
        let s = json_line_at("T", &m, Some(-1250.5));
        assert!(s.ends_with("\"LeakNow\":6},\"FreqOffsetHz\":-1250.5}"), "{s}");
    }

    #[test]
    fn rfc3339_known_values() {
        assert_eq!(rfc3339_utc(0, 0), "1970-01-01T00:00:00.000000000Z");
        // 2000-02-29 12:34:56 (うるう日)
        assert_eq!(rfc3339_utc(951_827_696, 5), "2000-02-29T12:34:56.000000005Z");
        // 2026-11-05 00:00:00
        assert_eq!(rfc3339_utc(1_793_836_800, 0), "2026-11-05T00:00:00.000000000Z");
    }
}
