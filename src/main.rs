// r900rs - Neptune R900 decoder in Rust
// Copyright (C) 2014 Douglas Hall (rtlamr)
// Copyright (C) 2026 Meku-30
// Derived from rtlamr (https://github.com/bemasher/rtlamr): main.go, flags.go.
// Modified 2026-11: ported from Go to Rust and restructured for lower CPU use;
// decoding results are kept identical to rtlamr v0.9.6.
// SPDX-License-Identifier: AGPL-3.0-or-later
//! r900rs: rtlamr -msgtype=r900 の置き換え。
mod decoder;
mod freq;
mod gf;
mod output;
mod r900;
mod rtltcp;
mod synth;

use decoder::{Cfg, Decoder};
use output::{json_line, Dedup};
use r900::Parser;
use std::io::{ErrorKind, Read, Write};
use std::path::PathBuf;
use std::time::{Duration, Instant};

const DEFAULT_CENTER_FREQ: u32 = 912_380_000;
const DATA_RATE: u32 = 32_768;
const VALID_SYMBOL_LENGTHS: [usize; 10] = [8, 32, 40, 48, 56, 64, 72, 80, 88, 96];

/// rtlamr と同じ -name=value の形の起動時の指定
#[derive(Debug, PartialEq)]
pub struct Args {
    pub server: String,
    pub msgtype: String,
    pub filterid: Vec<u32>,
    pub format: String,
    pub centerfreq: u32,
    pub symbollength: usize,
    pub tee: Option<PathBuf>,
    /// 録音の上限 (バイト)。達したら録音だけやめる。None は上限なし
    pub teemax: Option<u64>,
    pub reserve: Option<String>,
    /// false なら解読をせず、配り直し・録音だけ (最初の録音用)
    pub decode: bool,
    /// 比べる道具・速さを測るときだけ: rtl_tcp の代わりにファイルから IQ を読む (終わりで終了コード 0)
    pub file: Option<PathBuf>,
    /// rtlamr と同じ名前。false でチューナーの自動 (AGC)。true / 未指定は手動
    pub tunergainmode: Option<bool>,
    /// 固定のゲイン (dB)。rtlamr と同じ名前
    pub tunergain: Option<f64>,
}

/// rtl_tcp に送るゲインの命令
#[derive(Debug, PartialEq)]
pub enum GainCmd {
    /// 0x03。true = 手動
    Mode(bool),
    /// 0x04。0.1 dB 単位 (負もある: E4000・FC0012 など)
    Gain(i32),
}

/// rtl_tcp に送るゲインの命令。何も指定しなければ rtlamr と同じく手動だけを送る。
/// 注意: そのとき librtlsdr の R820T はゲインを 0 (一番低い感度) にする (r820t_set_gain_mode が r82xx_set_gain(manual, 0) を呼ぶ)。
/// 固定のゲインは手動 → 値の順に送る (rtlamr は指定を名前の辞書順に送るので tunergain → tunergainmode になり、
/// -tunergainmode=true を足すと値が 0 に戻る。r900rs はその順にしない)
/// 値は rtlamr (uint32(tunergain * 10)) と同じく 0 の方へ切り捨てる。範囲はチューナーによるので広めに -30〜60 dB
pub fn gain_commands(a: &Args) -> Result<Vec<GainCmd>, String> {
    match (a.tunergainmode, a.tunergain) {
        (Some(false), Some(_)) => Err("-tunergainmode=false (自動) と -tunergain は同時に使えない".into()),
        (Some(false), None) => Ok(vec![GainCmd::Mode(false)]),
        (_, Some(db)) if (-30.0..=60.0).contains(&db) => Ok(vec![GainCmd::Mode(true), GainCmd::Gain((db * 10.0) as i32)]),
        (_, Some(db)) => Err(format!("tunergain は -30〜60 dB: {db}")),
        (_, None) => Ok(vec![GainCmd::Mode(true)]),
    }
}

impl Args {
    pub fn parse<I: IntoIterator<Item = String>>(it: I) -> Result<Args, String> {
        let mut a = Args {
            server: "127.0.0.1:1234".into(),
            msgtype: "r900".into(),
            filterid: Vec::new(),
            format: "plain".into(),
            centerfreq: DEFAULT_CENTER_FREQ,
            symbollength: 72,
            tee: None,
            teemax: None,
            reserve: None,
            decode: true,
            file: None,
            tunergainmode: None,
            tunergain: None,
        };
        for arg in it {
            let body = arg
                .strip_prefix("--")
                .or_else(|| arg.strip_prefix('-'))
                .filter(|b| !b.starts_with('-'))
                .ok_or_else(|| format!("- か -- で始まっていない: {arg}"))?;
            let (name, val) = body
                .split_once('=')
                .ok_or_else(|| format!("-name=value の形ではない: {arg}"))?;
            match name {
                "server" => a.server = val.into(),
                "msgtype" => {
                    if val != "r900" {
                        return Err(format!("msgtype は r900 だけ: {val}"));
                    }
                    a.msgtype = val.into();
                }
                "filterid" => {
                    a.filterid = val
                        .split(',')
                        .filter(|s| !s.is_empty())
                        .map(|s| s.parse::<u32>().map_err(|e| format!("filterid {s}: {e}")))
                        .collect::<Result<_, _>>()?;
                }
                "format" => a.format = val.into(),
                "centerfreq" => a.centerfreq = val.parse().map_err(|e| format!("centerfreq: {e}"))?,
                "symbollength" => {
                    let n: usize = val.parse().map_err(|e| format!("symbollength: {e}"))?;
                    if !VALID_SYMBOL_LENGTHS.contains(&n) {
                        return Err(format!("symbollength に使えない値: {n}"));
                    }
                    a.symbollength = n;
                }
                "tee" => a.tee = Some(PathBuf::from(val)),
                "teemax" => {
                    let n: u64 = val.parse().map_err(|e| format!("teemax: {e}"))?;
                    if n == 0 {
                        return Err("teemax は 1 以上".into());
                    }
                    a.teemax = Some(n);
                }
                "reserve" => a.reserve = Some(val.into()),
                "decode" => {
                    a.decode = match val {
                        "true" => true,
                        "false" => false,
                        _ => return Err(format!("decode は true か false: {val}")),
                    }
                }
                "file" => a.file = Some(PathBuf::from(val)),
                "tunergainmode" => {
                    a.tunergainmode = Some(match val {
                        "true" => true,
                        "false" => false,
                        _ => return Err(format!("tunergainmode は true か false: {val}")),
                    })
                }
                "tunergain" => a.tunergain = Some(val.parse().map_err(|e| format!("tunergain: {e}"))?),
                _ => return Err(format!("知らない指定: {arg}")),
            }
        }
        gain_commands(&a)?; // ゲインの誤りはつなぐ前に (終了コード 2)。つないでからだと rtl_tcp が止まったのと見分けられない
        Ok(a)
    }

    pub fn sample_rate(&self) -> u32 {
        DATA_RATE * self.symbollength as u32
    }
}

fn main() {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    if argv.first().map(String::as_str) == Some("replay") {
        if let Err(e) = replay(&argv[1..]) {
            eprintln!("r900rs replay: {e}");
            std::process::exit(1);
        }
        return;
    }
    let args = match Args::parse(argv) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("r900rs: {e}");
            std::process::exit(2);
        }
    };
    if let Err(e) = run(&args) {
        eprintln!("r900rs: {e}");
        std::process::exit(1);
    }
}

/// UNIX 時刻 (秒) → "YYYYmmddTHHMMSS" (UTC、録音のファイル名に挟む)
fn utc_stamp(secs: u64) -> String {
    output::rfc3339_utc(secs, 0)[..19].replace(['-', ':'], "")
}

/// a.iq → a-<stamp>.iq (拡張子がなければ後ろに)
fn stamped(path: &std::path::Path, stamp: &str) -> PathBuf {
    let stem = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let name = match path.extension() {
        Some(e) => format!("{stem}-{stamp}.{}", e.to_string_lossy()),
        None => format!("{stem}-{stamp}"),
    };
    path.with_file_name(name)
}

/// 録音 (-tee)。書けなくなったら・上限 (-teemax) に達したら 1 回だけ stderr に出して、録音だけやめる (解読は続ける)
struct Tee {
    f: Option<std::fs::File>,
    max: Option<u64>,
    written: u64,
}

impl Tee {
    /// 前の録音は上書きしない (2026-11-05)。あれば拡張子の前に stamp を挟んだ名前で作る。作ったファイルの名前も返す
    fn open(path: &std::path::Path, max: Option<u64>, stamp: &str) -> std::io::Result<(Tee, PathBuf)> {
        use std::os::unix::fs::OpenOptionsExt;
        // 近所の電波が入っているので本人だけが読める
        let new = |p: &std::path::Path| std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(p);
        let (f, p) = match new(path) {
            Ok(f) => (f, path.to_path_buf()),
            Err(e) if e.kind() == ErrorKind::AlreadyExists => {
                let p = stamped(path, stamp);
                (new(&p)?, p)
            }
            Err(e) => return Err(e),
        };
        Ok((Tee { f: Some(f), max, written: 0 }, p))
    }

    fn write(&mut self, bytes: &[u8]) {
        let Some(f) = self.f.as_mut() else { return };
        let room = self.max.map_or(u64::MAX, |m| m - self.written);
        let n = (bytes.len() as u64).min(room) as usize;
        if let Err(e) = f.write_all(&bytes[..n]) {
            eprintln!("tee を書けない (録音をやめる。解読は続ける): {e}");
            self.f = None;
            return;
        }
        self.written += n as u64;
        if self.max == Some(self.written) {
            eprintln!("tee が上限 {} バイトに達した (録音をやめる。解読は続ける)", self.written);
            self.f = None;
        }
    }
}

/// 1 ブロックの処理: 録音・配り直し・(解読して出力)
struct Pipe {
    cfg: Cfg,
    fs: f64,
    decoder: Decoder,
    parser: Parser,
    dedup: Dedup,
    decode: bool,
    tee: Option<Tee>,
    reserve: Option<rtltcp::Reserve>,
    /// 直近の生の IQ (中心からのずれの計算用。見つかった電文の区間は直前のブロックにまたがる)
    hist: Vec<u8>,
}

impl Pipe {
    fn new(cfg: &Cfg, filterid: Vec<u32>, decode: bool, tee: Option<Tee>, reserve: Option<rtltcp::Reserve>) -> Pipe {
        Pipe {
            cfg: cfg.clone(),
            fs: (DATA_RATE as usize * cfg.chip_length) as f64,
            decoder: Decoder::new(cfg.clone()),
            parser: Parser::new(cfg.clone()),
            dedup: Dedup::new(filterid),
            decode,
            tee,
            reserve,
            hist: Vec::new(),
        }
    }

    #[cfg(test)]
    fn tee_dead(&self) -> bool {
        self.tee.as_ref().map_or(false, |t| t.f.is_none())
    }

    #[cfg(test)]
    fn reserve_dropped(&self) -> u64 {
        self.reserve.as_ref().map_or(0, |r| r.dropped())
    }

    fn process<W: Write>(&mut self, block: &[u8], out: &mut W) -> std::io::Result<()> {
        // 先に配る (rtlamr への遅れを増やさない)
        if let Some(r) = self.reserve.as_mut() {
            r.send(block);
        }
        if let Some(t) = self.tee.as_mut() {
            t.write(block);
        }
        if !self.decode {
            return Ok(());
        }
        let c = &self.cfg;
        // 直前の packet_length + symbol_length 個とこのブロックが残るようにする (たまに前を捨てる)
        let keep = 2 * (c.packet_length + c.symbol_length + c.block_size);
        if self.hist.len() >= 2 * keep {
            self.hist.drain(..self.hist.len() - keep);
        }
        self.hist.extend_from_slice(block);

        let pkts = self.decoder.decode(block).to_vec();
        self.parser.push_signal(&self.decoder.signal()[c.symbol_length..]);
        let msgs = self.parser.parse(&pkts);
        // このブロックの先頭のサンプル (hist の中の位置)。Quantized[j] は
        // このブロックの先頭 - symbol_length - packet_length + j 番目のサンプルから始まる窓
        let cur = (self.hist.len() >> 1) as isize - c.block_size as isize;
        for m in self.dedup.filter(msgs) {
            let s = cur - (c.symbol_length + c.packet_length) as isize + m.q_idx as isize;
            let off = if s >= 0 { freq::offset_hz(&self.hist, s as usize, c.packet_length, self.fs) } else { f64::NAN };
            writeln!(out, "{}", json_line(&m, Some(off)))?;
            out.flush()?;
        }
        Ok(())
    }
}

fn run(args: &Args) -> std::io::Result<()> {
    if args.format != "json" {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("format は json だけ対応: {}", args.format),
        ));
    }
    let cfg = Cfg::new(args.symbollength);

    let mut file = match &args.file {
        Some(p) => Some(std::fs::File::open(p)?),
        None => None,
    };
    let mut c = match &file {
        Some(_) => None,
        None => {
            let mut c = rtltcp::Client::connect(&args.server)?;
            c.set_center_freq(args.centerfreq)?;
            c.set_sample_rate(args.sample_rate())?;
            for g in gain_commands(&args).map_err(std::io::Error::other)? {
                match g {
                    GainCmd::Mode(manual) => c.set_gain_mode(manual)?,
                    GainCmd::Gain(tenths) => c.set_gain(tenths)?,
                }
            }
            Some(c)
        }
    };
    // 開けなくても (ディレクトリがない等) 終わらず、録音なしで解読は続ける (2026-11-05)
    let tee = args.tee.as_ref().and_then(|p| {
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default();
        match Tee::open(p, args.teemax, &utc_stamp(now.as_secs())) {
            Ok((t, got)) => {
                eprintln!("tee: {} に録音する", got.display());
                Some(t)
            }
            Err(e) => {
                eprintln!("tee を開けない (録音なしで続ける): {}: {e}", p.display());
                None
            }
        }
    });
    // rtlamr には本物の rtl_tcp と同じ頭を見せる
    let reserve = match &args.reserve {
        Some(addr) => {
            let head = c.as_ref().map_or_else(|| rtltcp::header_for(5, 29), |c| rtltcp::header_for(c.tuner, c.gain_count));
            Some(rtltcp::Reserve::start(addr, head)?)
        }
        None => None,
    };
    let mut pipe = Pipe::new(&cfg, args.filterid.clone(), args.decode, tee, reserve);

    // rtlamr と同じく 1 ブロック = 2 * block_size バイト (I/Q)
    let mut block = vec![0u8; cfg.block_size << 1];
    let rate_floor = args.sample_rate() as usize * 9 / 10;
    let mut tick = Instant::now();
    let mut bytes_read = 0usize;
    let stdout = std::io::stdout();
    loop {
        if let Some(f) = file.as_mut() {
            // ファイルの終わり (最後の端数は捨てる。rtlamr と同じ)
            if !read_full_block(f, &mut block)? {
                return Ok(());
            }
        } else if let Err(e) = c.as_mut().unwrap().read_exact_block(&mut block) {
            if matches!(e.kind(), ErrorKind::TimedOut | ErrorKind::WouldBlock) {
                eprintln!("rtl_tcp から {} 秒届かない", rtltcp::IO_TIMEOUT.as_secs());
            }
            return Err(e);
        }
        bytes_read += block.len();
        // 1 秒ごとに届いたサンプル数が 9 割に満たなければ知らせる (rtlamr と同じ)
        if tick.elapsed() >= Duration::from_secs(1) {
            if bytes_read >> 1 < rate_floor {
                eprintln!("not keeping up with rtl_tcp rate={}", bytes_read >> 1);
            }
            bytes_read = 0;
            tick = Instant::now();
        }
        pipe.process(&block, &mut stdout.lock())?;
    }
}

/// ブロックを埋める。足りないまま終わったら false
fn read_full_block(f: &mut std::fs::File, buf: &mut [u8]) -> std::io::Result<bool> {
    let mut off = 0;
    while off < buf.len() {
        match f.read(&mut buf[off..]) {
            Ok(0) => return Ok(false),
            Ok(n) => off += n,
            Err(e) if e.kind() == ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(true)
}

/// `r900rs replay <ファイル> [-listen=127.0.0.1:1235]`: 録音を rtl_tcp のふりをして流す (比べる道具)。
/// 1 人だけ受け付け、頭を送り、命令は読んで捨て、流し終えたら閉じる
fn replay(argv: &[String]) -> std::io::Result<()> {
    let bad = |m: String| std::io::Error::new(ErrorKind::InvalidInput, m);
    let mut path = None;
    let mut listen = "127.0.0.1:1235".to_string();
    for a in argv {
        if let Some(v) = a.strip_prefix("-listen=") {
            listen = v.into();
        } else if path.is_none() && !a.starts_with('-') {
            path = Some(PathBuf::from(a));
        } else {
            return Err(bad(format!("知らない指定: {a}")));
        }
    }
    let path = path.ok_or_else(|| bad("使い方: r900rs replay <ファイル> [-listen=127.0.0.1:1235]".into()))?;
    let mut f = std::fs::File::open(&path)?;
    // 録音を外に流さない: 名前が引けたアドレスが全部ループバックのときだけ待ち受ける (bind の前に確かめる)
    use std::net::ToSocketAddrs;
    let addrs: Vec<std::net::SocketAddr> = listen.to_socket_addrs()?.collect();
    if addrs.is_empty() || !addrs.iter().all(|a| a.ip().is_loopback()) {
        return Err(bad(format!("listen はループバックだけ: {listen}")));
    }
    let srv = rtltcp::Server::bind(&addrs[..1])?;
    eprintln!("r900rs replay: listening {}", srv.local_addr()?);
    let mut conn = srv.accept()?;
    let mut buf = vec![0u8; 1 << 20];
    let mut sent: u64 = 0;
    loop {
        let n = match f.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) if e.kind() == ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        };
        conn.stream.write_all(&buf[..n])?;
        sent += n as u64;
    }
    // 命令を読む側が複製を持っているので、shutdown で相手に終わりを知らせる
    conn.stream.shutdown(std::net::Shutdown::Write)?;
    eprintln!("r900rs replay: sent {sent} bytes");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(v: &[&str]) -> Result<Args, String> {
        Args::parse(v.iter().map(|s| s.to_string()))
    }

    #[test]
    fn gain_commands_like_rtlamr_but_value_last() {
        let p = |v: &[&str]| Args::parse(v.iter().map(|s| s.to_string())).unwrap();
        use GainCmd::*;
        assert_eq!(gain_commands(&p(&[])).unwrap(), vec![Mode(true)]); // rtlamr の既定 (一番低い感度)
        assert_eq!(gain_commands(&p(&["-tunergainmode=false"])).unwrap(), vec![Mode(false)]);
        assert_eq!(gain_commands(&p(&["-tunergain=49.6"])).unwrap(), vec![Mode(true), Gain(496)]);
        assert_eq!(gain_commands(&p(&["-tunergainmode=true", "-tunergain=40.2"])).unwrap(), vec![Mode(true), Gain(402)]);
        assert_eq!(gain_commands(&p(&["-tunergain=2.96"])).unwrap()[1], Gain(29)); // rtlamr と同じ切り捨て (四捨五入なら 30)
        assert_eq!(gain_commands(&p(&["-tunergain=-9.9"])).unwrap()[1], Gain(-99));
        // 誤りは指定を読む時点で (つなぐ前に)
        assert!(Args::parse(["-tunergainmode=false".to_string(), "-tunergain=40".to_string()]).is_err());
        assert!(Args::parse(["-tunergain=99".to_string()]).is_err());
        assert!(Args::parse(["-tunergain=NaN".to_string()]).is_err());
        assert!(Args::parse(["-tunergain=x".to_string()]).is_err());
        assert!(Args::parse(["-tunergainmode=1".to_string()]).is_err());
    }

    #[test]
    fn args_parse_rtlamr_style() {
        let a = p(&[
            "-server=127.0.0.1:1234",
            "-msgtype=r900",
            "-filterid=123,456",
            "-format=json",
            "-centerfreq=921820000",
            "-symbollength=96",
        ])
        .unwrap();
        assert_eq!(a.server, "127.0.0.1:1234");
        assert_eq!(a.msgtype, "r900");
        assert_eq!(a.filterid, vec![123, 456]);
        assert_eq!(a.format, "json");
        assert_eq!(a.centerfreq, 921_820_000);
        assert_eq!(a.symbollength, 96);
        assert_eq!(a.sample_rate(), 32_768 * 96);
        assert!(p(&["-msgtype=scm"]).is_err());
        // Go の flag と同じく - か -- だけ受ける
        assert!(p(&["--server=127.0.0.1:1"]).is_ok());
        assert!(p(&["---server=127.0.0.1:1"]).is_err());
        assert!(p(&["server=127.0.0.1:1"]).is_err());
    }

    #[test]
    fn args_defaults() {
        let a = p(&[]).unwrap();
        assert_eq!(a.centerfreq, 912_380_000);
        assert_eq!(a.sample_rate(), 2_359_296);
        assert!(a.filterid.is_empty() && a.tee.is_none() && a.reserve.is_none());
        assert!(a.file.is_none());
        assert!(a.decode);
        assert!(!p(&["-decode=false"]).unwrap().decode);
        assert!(p(&["-decode=no"]).is_err());
        assert_eq!(p(&["-file=/x.iq"]).unwrap().file, Some(PathBuf::from("/x.iq")));
        assert!(a.teemax.is_none());
        assert_eq!(p(&["-teemax=9000000000"]).unwrap().teemax, Some(9_000_000_000));
        assert!(p(&["-teemax=0"]).is_err());
        assert!(p(&["-teemax=x"]).is_err());
    }

    #[test]
    fn tee_stamped_name() {
        assert_eq!(utc_stamp(0), "19700101T000000");
        assert_eq!(utc_stamp(1_793_923_200), "20261106T000000");
        let s = "20261105T220000";
        assert_eq!(stamped(std::path::Path::new("/d/a.iq"), s), PathBuf::from("/d/a-20261105T220000.iq"));
        assert_eq!(stamped(std::path::Path::new("/d/a"), s), PathBuf::from("/d/a-20261105T220000"));
    }

    #[test]
    fn tee_does_not_overwrite_existing() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("r900rs-tee-keep-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("a.iq");
        std::fs::write(&path, b"old").unwrap();
        let (mut tee, got) = Tee::open(&path, None, "20261105T220000").unwrap();
        tee.write(b"new");
        drop(tee);
        assert_eq!(got, dir.join("a-20261105T220000.iq"));
        assert_eq!(std::fs::read(&path).unwrap(), b"old", "前の録音を上書きした");
        assert_eq!(std::fs::read(&got).unwrap(), b"new");
        assert_eq!(std::fs::metadata(&got).unwrap().permissions().mode() & 0o777, 0o600);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn tee_missing_dir_is_error() {
        assert!(Tee::open(std::path::Path::new("/nonexistent-r900rs-dir/a.iq"), None, "x").is_err());
    }

    #[test]
    fn tee_stops_at_max() {
        let cfg = Cfg::new(72);
        let dir = std::env::temp_dir().join(format!("r900rs-tee-max-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("a.iq");
        let blk = cfg.block_size << 1;
        let max = (3 * blk + 100) as u64;
        let (tee, _) = Tee::open(&path, Some(max), "x").unwrap();
        let mut pipe = Pipe::new(&cfg, vec![], true, Some(tee), None);
        for _ in 0..5 {
            pipe.process(&quiet_block(&cfg), &mut Vec::new()).unwrap();
        }
        assert!(pipe.tee_dead());
        assert_eq!(std::fs::metadata(&path).unwrap().len(), max);
        std::fs::remove_dir_all(&dir).ok();
    }

    /// 1 ブロック分の IQ (偽の電波。idx 番目のブロックに電文の頭を置く)
    fn quiet_block(cfg: &Cfg) -> Vec<u8> {
        vec![128u8; cfg.block_size << 1]
    }

    #[test]
    fn tee_writes_identical_bytes() {
        let cfg = Cfg::new(72);
        let path = std::env::temp_dir().join(format!("r900rs-tee-{}.iq", std::process::id()));
        std::fs::remove_file(&path).ok();
        let (tee, got_path) = Tee::open(&path, None, "20261105T220000").unwrap();
        assert_eq!(got_path, path);
        let mut pipe = Pipe::new(&cfg, vec![], false, Some(tee), None);
        let mut want = Vec::new();
        for k in 0..5u8 {
            let b: Vec<u8> = quiet_block(&cfg).iter().map(|v| v.wrapping_add(k)).collect();
            want.extend(&b);
            pipe.process(&b, &mut Vec::new()).unwrap();
        }
        drop(pipe);
        let got = std::fs::read(&path).unwrap();
        std::fs::remove_file(&path).ok();
        assert!(got == want, "録音がそのまま書かれていない");
    }

    #[test]
    fn tee_failure_stops_only_recording() {
        let cfg = Cfg::new(72);
        // /dev/full は書くと ENOSPC
        let f = std::fs::OpenOptions::new().write(true).open("/dev/full").unwrap();
        let tee = Tee { f: Some(f), max: None, written: 0 };
        let mut pipe = Pipe::new(&cfg, vec![], true, Some(tee), None);
        for _ in 0..3 {
            pipe.process(&quiet_block(&cfg), &mut Vec::new()).unwrap();
        }
        assert!(pipe.tee_dead());
    }

    #[test]
    fn slow_client_does_not_block_decoder() {
        let cfg = Cfg::new(72);
        let res = rtltcp::Reserve::start("127.0.0.1:0", [0u8; 12]).unwrap();
        let addr = res.local_addr().to_string();
        // つないだまま読まない相手
        let _slow = std::net::TcpStream::connect(&addr).unwrap();
        std::thread::sleep(Duration::from_millis(200));
        let mut pipe = Pipe::new(&cfg, vec![], true, None, Some(res));
        let blocks = 10 * (DATA_RATE as usize * 72) / cfg.block_size;
        let b = quiet_block(&cfg);
        let t = Instant::now();
        for _ in 0..blocks {
            pipe.process(&b, &mut Vec::new()).unwrap();
        }
        let el = t.elapsed();
        assert!(el < Duration::from_secs(1), "10 秒分に {el:?}");
        assert!(pipe.reserve_dropped() > 0, "あふれて捨てた数が数えられていない");
    }

    #[test]
    fn offset_reported_for_packet() {
        let cfg = Cfg::new(72);
        let fs = (DATA_RATE * 72) as f64;
        for hz in [150_000.0, -300_000.0] {
            let pkt = synth::packet_iq(&synth::encode(&r900::R900 { id: 4_000_000_001, consumption: 42, ..Default::default() }), 72, 0.9, hz, fs);
            let mut stream = vec![128u8; 2 * cfg.block_size * 12];
            let at = 2 * (3 * cfg.block_size + 1234);
            stream[at..at + pkt.len()].copy_from_slice(&pkt);
            let mut pipe = Pipe::new(&cfg, vec![], true, None, None);
            let mut out = Vec::new();
            for blk in stream.chunks_exact(2 * cfg.block_size) {
                pipe.process(blk, &mut out).unwrap();
            }
            let text = String::from_utf8(out).unwrap();
            let lines: Vec<&str> = text.lines().collect();
            assert_eq!(lines.len(), 1, "{text}");
            let f: f64 = lines[0].split("\"FreqOffsetHz\":").nth(1).unwrap().trim_end_matches('}').parse().unwrap();
            assert!((f - hz).abs() < 2_000.0, "want {hz} got {f}");
        }
    }
}
