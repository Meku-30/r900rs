# r900rs

English | [日本語](README.ja.md)

A decoder for Neptune R900 water meter broadcasts (915 MHz ISM band) received with an RTL-SDR. It is a Rust port of the R900 decoder in [rtlamr](https://github.com/bemasher/rtlamr) (Go), and its goal is to **emit the same messages, in the same order, as rtlamr does for the same signal**, at a fraction of the CPU cost. It has no crate dependencies (standard library only).

Not affiliated with Neptune Technology Group or with the rtlamr project.

## Differences from rtlamr

- R900 only (`-msgtype=r900`), JSON output only (`-format=json`).
- About 1/20 of rtlamr's CPU use. rtlamr runs the R900 4-chip filter over the whole buffer on every block, even when nothing is being received; r900rs computes it only for blocks where the preamble search found a candidate, and only at the 42 positions the payload actually uses. Results are identical. CPU seconds per second of signal (x86_64 laptop, 600 s synthetic signal):

  | | symbollength 72 | symbollength 96 |
  |---|---|---|
  | rtlamr v0.9.6 | 0.082 | 0.123 |
  | r900rs | 0.004 | 0.005 |

- In the JSON output, `Time` is UTC, and `Offset` / `Length` are always 0 (rtlamr reports local time, a file offset and a buffer length). The field names and order inside `Message` are the same as rtlamr's. `FreqOffsetHz` (estimated offset from the receive center frequency) is added.
- rtlamr v0.9.6 ignores `-server` and always connects to `127.0.0.1:1234`; r900rs honours `-server`.

## Usage

Start rtl_tcp, then connect:

```sh
rtl_tcp -a 127.0.0.1 &
r900rs -server=127.0.0.1:1234 -msgtype=r900 -format=json [-filterid=<ID>,...] [-tunergainmode=false]
```

| Flag | Meaning |
|---|---|
| `-server=<host:port>` | rtl_tcp address (default `127.0.0.1:1234`) |
| `-filterid=<ID>,...` | Only print these meter IDs |
| `-centerfreq=<Hz>` | Center frequency (default 912,380,000) |
| `-symbollength=<n>` | 8, 32, 40, 48, 56, 64, 72, 80, 88 or 96 (default 72). Sample rate is 32,768 × n |
| `-tunergainmode=false` | Tuner automatic gain (AGC) |
| `-tunergain=<dB>` | Fixed gain (-30 to 60, truncated to 0.1 dB like rtlamr). Sets manual mode, then the value |
| `-tee=<file>` | Write the received IQ samples to a file. If the file exists, a timestamp is inserted into the name |
| `-teemax=<bytes>` | Stop recording (only recording) after this many bytes |
| `-reserve=<host:port>` | Re-serve the received IQ in rtl_tcp format (loopback only), e.g. to run rtlamr side by side |
| `-decode=false` | Do not decode (re-serve / record only) |
| `-file=<file>` | Read IQ from a file instead of rtl_tcp |

rtlamr's other tuner flags (`-agcmode`, `-gainbyindex`, `-freqcorrection`, ...) are not supported and are rejected at startup.

If nothing arrives from rtl_tcp for 5 seconds, or the connection closes, r900rs exits with status 1 (the caller is expected to restart it).

**About gain:** with no gain flag, r900rs does what rtlamr does and only sends "manual gain mode". With an R820T tuner, librtlsdr then sets the gain to 0 — the lowest sensitivity — because `r820t_set_gain_mode` calls `r82xx_set_gain(manual, 0)`. If reception is weak, pass `-tunergainmode=false` or `-tunergain=<dB>`. With rtlamr, use `-tunergain` alone for a fixed gain: rtlamr sends flags in lexicographic order, so adding `-tunergainmode=true` switches to manual mode *after* the value and resets it to 0.

## Verifying against rtlamr

```sh
cargo test --release
cargo build --release
# Synthetic signal: 8-bit IQ with R900 packets (random fake IDs and readings, with Reed-Solomon parity)
target/release/r900synth -o /tmp/a.iq -seconds=600 -symbollength=72 -seed=1
# Feed the same file to rtlamr and r900rs and compare the message sequences, in order (IDs are not printed)
RTLAMR=/path/to/rtlamr tools/parity.sh /tmp/a.iq 72
```

`r900synth` flags: `-seconds`, `-symbollength`, `-seed`, `-noise=<0..1>` (1 = no packets), `-cw` (constant signal), `-offset=<Hz>`. `r900rs replay <file> [-listen=127.0.0.1:1235]` plays a file once as a fake rtl_tcp. `parity.sh` uses 127.0.0.1:1234 for rtlamr. The comparison reference is rtlamr v0.9.6 (`go install github.com/bemasher/rtlamr@v0.9.6`).

## Privacy

`-reserve` and `replay` only listen on loopback, but other users and host-network containers on the same machine can still connect to them. Recordings of real signals contain the IDs and consumption of neighbouring meters. Do not publish them, and only use the decoded data of meters you are entitled to read. Rules on receiving and using these broadcasts differ by country.

## License

AGPL-3.0-or-later (`LICENSE`). Contains code derived from rtlamr, Copyright (C) 2014 Douglas Hall, licensed under the GNU Affero General Public License v3 or later. Each derived source file names the rtlamr file it is based on and states that it was modified (ported from Go to Rust in 2026-11).
