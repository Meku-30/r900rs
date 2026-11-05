// r900rs - Neptune R900 decoder in Rust
// Copyright (C) 2014 Douglas Hall (rtlamr)
// Copyright (C) 2026 Meku-30
// Derived from rtlamr (https://github.com/bemasher/rtlamr): main.go and github.com/bemasher/rtltcp (rtl_tcp protocol).
// Modified 2026-11: ported from Go to Rust and restructured for lower CPU use;
// decoding results are kept identical to rtlamr v0.9.6.
// SPDX-License-Identifier: AGPL-3.0-or-later
//! rtl_tcp のクライアント (rtlamr の入力) と、仮のサーバー (配り直し用)。
//! つないだ直後にサーバーが 12 バイト (RTL0 + tuner u32 BE + gain 数 u32 BE) を送る。
//! 命令は 5 バイト (命令 u8 + 値 u32 BE)。

use std::collections::VecDeque;
use std::io::{self, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream, ToSocketAddrs};
use std::sync::{Arc, Condvar, Mutex};
use std::thread;
use std::time::{Duration, Instant};

/// rtlamr の SetDeadline (5 秒) と同じ。接続と読み取りの待ち時間
pub const IO_TIMEOUT: Duration = Duration::from_secs(5);

const CMD_CENTER_FREQ: u8 = 0x01;
const CMD_SAMPLE_RATE: u8 = 0x02;
const CMD_GAIN_MODE: u8 = 0x03;
const CMD_GAIN: u8 = 0x04;

/// つないだ相手に送る頭 (tuner = R820T の 5、gain 数 = 29)
fn header() -> [u8; 12] {
    header_for(5, 29)
}

/// 頭 (RTL0 + tuner + gain 数)。配り直しでは本物の rtl_tcp の値をそのまま使う
pub fn header_for(tuner: u32, gain_count: u32) -> [u8; 12] {
    let mut h = [0u8; 12];
    h[..4].copy_from_slice(b"RTL0");
    h[4..8].copy_from_slice(&tuner.to_be_bytes());
    h[8..12].copy_from_slice(&gain_count.to_be_bytes());
    h
}

pub struct Client {
    stream: TcpStream,
    pub tuner: u32,
    pub gain_count: u32,
}

impl Client {
    pub fn connect(addr: &str) -> io::Result<Client> {
        Client::connect_with(addr, IO_TIMEOUT)
    }

    /// 接続・頭の読み取り・以後の読み取りに timeout を掛ける (rtl_tcp が黙ったら TimedOut / WouldBlock)
    pub fn connect_with(addr: &str, timeout: Duration) -> io::Result<Client> {
        let sa = addr
            .to_socket_addrs()?
            .next()
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "アドレスを解決できない"))?;
        let mut stream = TcpStream::connect_timeout(&sa, timeout)?;
        stream.set_read_timeout(Some(timeout))?;
        stream.set_nodelay(true)?;
        let mut h = [0u8; 12];
        stream.read_exact(&mut h)?;
        if &h[..4] != b"RTL0" {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "rtl_tcp の頭が RTL0 ではない"));
        }
        let tuner = u32::from_be_bytes([h[4], h[5], h[6], h[7]]);
        let gain_count = u32::from_be_bytes([h[8], h[9], h[10], h[11]]);
        Ok(Client { stream, tuner, gain_count })
    }

    fn command(&mut self, cmd: u8, param: u32) -> io::Result<()> {
        let mut b = [0u8; 5];
        b[0] = cmd;
        b[1..].copy_from_slice(&param.to_be_bytes());
        self.stream.write_all(&b)
    }

    pub fn set_center_freq(&mut self, hz: u32) -> io::Result<()> {
        self.command(CMD_CENTER_FREQ, hz)
    }

    pub fn set_sample_rate(&mut self, hz: u32) -> io::Result<()> {
        self.command(CMD_SAMPLE_RATE, hz)
    }

    /// manual = true で 1 (rtlamr は -tunergain 未指定でも manual にする)
    pub fn set_gain_mode(&mut self, manual: bool) -> io::Result<()> {
        self.command(CMD_GAIN_MODE, manual as u32)
    }

    /// 固定のゲイン (0.1 dB 単位)。librtlsdr の R820T はこれで手動に切り替えて値を入れる
    pub fn set_gain(&mut self, tenths_db: i32) -> io::Result<()> {
        self.command(CMD_GAIN, tenths_db as u32) // rtl_tcp は int として読む (負もある)
    }

    pub fn read_exact_block(&mut self, buf: &mut [u8]) -> io::Result<()> {
        self.stream.read_exact(buf)
    }
}

/// 配り直し用の仮の rtl_tcp (127.0.0.1 だけで使う)
pub struct Server {
    listener: TcpListener,
}

pub struct Conn {
    pub stream: TcpStream,
}

impl Server {
    pub fn bind<A: ToSocketAddrs>(addr: A) -> io::Result<Server> {
        Ok(Server { listener: TcpListener::bind(addr)? })
    }

    pub fn local_addr(&self) -> io::Result<std::net::SocketAddr> {
        self.listener.local_addr()
    }

    /// つないできた相手に頭を送り、命令は読んで捨てる
    pub fn accept(&self) -> io::Result<Conn> {
        self.accept_with(&header(), || {})
    }

    /// on_close は相手が閉じた (読み取りが終わった) ときに 1 回呼ぶ
    pub fn accept_with<F: FnOnce() + Send + 'static>(&self, head: &[u8; 12], on_close: F) -> io::Result<Conn> {
        let (mut stream, _) = self.listener.accept()?;
        stream.set_nodelay(true)?;
        stream.write_all(head)?;
        let mut rd = stream.try_clone()?;
        thread::spawn(move || {
            let mut buf = [0u8; 256];
            while matches!(rd.read(&mut buf), Ok(n) if n > 0) {}
            on_close();
        });
        Ok(Conn { stream })
    }
}

/// 相手 (rtlamr) への待ち行列の上限
pub const RESERVE_QUEUE_MAX: usize = 4 << 20;
const RESERVE_REPORT_EVERY: Duration = Duration::from_secs(60);

#[derive(Default)]
struct Queue {
    chunks: VecDeque<Vec<u8>>,
    bytes: usize,
    connected: bool,
    /// 今の相手の通し番号と、その相手が閉じたか (古い相手の通知で今の相手を切らない)
    conn_id: u64,
    closed: bool,
    dropped: u64,
}

struct Shared {
    q: Mutex<Queue>,
    cv: Condvar,
}

/// 受けたバイトを 1 人にそのまま渡す (127.0.0.1 などループバックだけ)。
/// `send` は待たない: 待ち行列が 4 MB を超えるなら捨てて数え、相手がいなければ捨てる。
/// 書き込みは相手ごとの別スレッド。相手が切れたら次を待つ。相手からの命令は読んで捨てる
pub struct Reserve {
    shared: Arc<Shared>,
    #[allow(dead_code)] // テストが使う
    addr: SocketAddr,
    reported: Instant,
    dropped_reported: u64,
}

impl Reserve {
    pub fn start(addr: &str, head: [u8; 12]) -> io::Result<Reserve> {
        // 近所の電波を外に流さない: 名前が引けたアドレスが全部ループバックでなければ拒む
        let addrs: Vec<SocketAddr> = addr.to_socket_addrs()?.collect();
        if addrs.is_empty() || !addrs.iter().all(|a| a.ip().is_loopback()) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("reserve はループバックだけ: {addr}"),
            ));
        }
        let srv = Server::bind(&addrs[..1])?;
        let local = srv.local_addr()?;
        let shared = Arc::new(Shared { q: Mutex::new(Queue::default()), cv: Condvar::new() });
        let sh = shared.clone();
        thread::spawn(move || loop {
            // 次の相手の番号を先に決める (つないですぐ閉じても通知が合う)
            let id = {
                let mut q = sh.q.lock().unwrap();
                q.conn_id += 1;
                q.closed = false;
                q.conn_id
            };
            let sh2 = sh.clone();
            let mut conn = match srv.accept_with(&head, move || {
                let mut q = sh2.q.lock().unwrap();
                if q.conn_id == id {
                    q.closed = true;
                    sh2.cv.notify_all();
                }
            }) {
                // 読まない相手に書き込み続けて止まらない (止まると次の相手を受け付けられない)
                Ok(c) if c.stream.set_write_timeout(Some(IO_TIMEOUT)).is_ok() => c,
                Ok(_) => continue,
                Err(_) => {
                    thread::sleep(Duration::from_millis(200));
                    continue;
                }
            };
            {
                let mut q = sh.q.lock().unwrap();
                q.connected = !q.closed;
            }
            loop {
                let chunk = {
                    let mut q = sh.q.lock().unwrap();
                    while q.chunks.is_empty() && !q.closed {
                        q = sh.cv.wait(q).unwrap();
                    }
                    if q.closed {
                        break;
                    }
                    let c = q.chunks.pop_front().unwrap();
                    q.bytes -= c.len();
                    c
                };
                if conn.stream.write_all(&chunk).is_err() {
                    break;
                }
            }
            let mut q = sh.q.lock().unwrap();
            q.connected = false;
            q.chunks.clear();
            q.bytes = 0;
        });
        Ok(Reserve { shared, addr: local, reported: Instant::now(), dropped_reported: 0 })
    }

    #[allow(dead_code)] // テストが使う
    pub fn local_addr(&self) -> SocketAddr {
        self.addr
    }

    /// 捨てたバイト数の合計
    #[allow(dead_code)] // テストが使う
    pub fn dropped(&self) -> u64 {
        self.shared.q.lock().unwrap().dropped
    }

    pub fn send(&mut self, bytes: &[u8]) {
        let total = {
            let mut q = self.shared.q.lock().unwrap();
            if q.connected {
                if q.bytes + bytes.len() > RESERVE_QUEUE_MAX {
                    q.dropped += bytes.len() as u64;
                } else {
                    q.bytes += bytes.len();
                    q.chunks.push_back(bytes.to_vec());
                    self.shared.cv.notify_one();
                }
            }
            q.dropped
        };
        if self.reported.elapsed() >= RESERVE_REPORT_EVERY {
            if total > self.dropped_reported {
                eprintln!("reserve dropped {}", total - self.dropped_reported);
                self.dropped_reported = total;
            }
            self.reported = Instant::now();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn client_sends_rtlamr_commands() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        let fake = thread::spawn(move || {
            let (mut s, _) = listener.accept().unwrap();
            s.write_all(&header()).unwrap();
            let mut got = [0u8; 20];
            s.read_exact(&mut got).unwrap();
            got
        });
        let mut c = Client::connect(&addr).unwrap();
        assert_eq!((c.tuner, c.gain_count), (5, 29));
        c.set_center_freq(912_380_000).unwrap();
        c.set_sample_rate(2_359_296).unwrap();
        c.set_gain_mode(true).unwrap();
        c.set_gain(496).unwrap();
        let got = fake.join().unwrap();
        let mut want = vec![0x01];
        want.extend(912_380_000u32.to_be_bytes());
        want.push(0x02);
        want.extend(2_359_296u32.to_be_bytes());
        want.extend([0x03, 0, 0, 0, 1]);
        want.extend([0x04, 0, 0, 0x01, 0xf0]);
        assert_eq!(got.to_vec(), want);
    }

    #[test]
    fn server_sends_header_and_drains_commands() {
        let srv = Server::bind("127.0.0.1:0").unwrap();
        let addr = srv.local_addr().unwrap().to_string();
        let t = thread::spawn(move || srv.accept().unwrap());
        let mut c = Client::connect(&addr).unwrap();
        c.set_gain_mode(true).unwrap();
        let mut conn = t.join().unwrap();
        conn.stream.write_all(&[1, 2, 3]).unwrap();
        let mut b = [0u8; 3];
        c.read_exact_block(&mut b).unwrap();
        assert_eq!(b, [1, 2, 3]);
    }

    #[test]
    fn reserve_forwards_bytes_and_survives_reconnect() {
        let mut res = Reserve::start("127.0.0.1:0", header_for(7, 9)).unwrap();
        let addr = res.local_addr().to_string();
        for round in 0..2u8 {
            let mut c = Client::connect(&addr).unwrap();
            assert_eq!((c.tuner, c.gain_count), (7, 9));
            c.set_gain_mode(true).unwrap();
            // つながったことが send に見えるまで待つ
            let mut got = [0u8; 4];
            for _ in 0..200 {
                res.send(&[round, 1, 2, 3]);
                c.stream.set_read_timeout(Some(Duration::from_millis(50))).unwrap();
                if c.read_exact_block(&mut got).is_ok() {
                    break;
                }
            }
            assert_eq!(got[1..], [1, 2, 3]);
        }
    }

    #[test]
    fn reserve_refuses_non_loopback() {
        assert!(Reserve::start("0.0.0.0:0", [0; 12]).is_err());
        assert!(Reserve::start("192.0.2.1:1234", [0; 12]).is_err());
    }

    #[test]
    fn silent_server_times_out() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        let fake = thread::spawn(move || {
            let (mut s, _) = listener.accept().unwrap();
            s.write_all(&header()).unwrap();
            thread::sleep(Duration::from_secs(2)); // 頭だけ送って黙る
        });
        let mut c = Client::connect_with(&addr, Duration::from_millis(200)).unwrap();
        let t = std::time::Instant::now();
        let e = c.read_exact_block(&mut [0u8; 16]).unwrap_err();
        assert!(matches!(e.kind(), io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock), "{e:?}");
        assert!(t.elapsed() < Duration::from_secs(1));
        drop(c);
        fake.join().unwrap();
    }
}
