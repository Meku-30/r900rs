# r900rs

[English](README.md) | 日本語

Neptune R900 (915 MHz 帯の水道メーター) の電文を、RTL-SDR の電波から読むプログラム。[rtlamr](https://github.com/bemasher/rtlamr) (Go) の R900 の解読を Rust に移したもので、**同じ電波から rtlamr と同じ電文を同じ順に出す**ことを目標にしている。依存のクレートはない (標準ライブラリだけ)。

## rtlamr との違い

- 読むのは R900 だけ (`-msgtype=r900`)。出力は `-format=json` だけ
- CPU がおよそ 20 分の 1。rtlamr は電文が来ていなくても R900 の 4 チップのフィルターを毎回バッファー全体にかけるが、r900rs はプリアンブルの候補が見つかったときだけ、使う 42 か所だけを計算する (結果は同じ)。1 秒の電波あたりの CPU 秒 (x86_64 のノート PC、600 秒の偽の電波):

  | | symbollength 72 | symbollength 96 |
  |---|---|---|
  | rtlamr v0.9.6 | 0.082 | 0.123 |
  | r900rs | 0.004 | 0.005 |

- JSON の `Time` は UTC、`Offset`・`Length` は常に 0 (rtlamr はローカル時刻・ファイルの位置・バッファーの長さ)。`Message` の欄の名前と順は rtlamr と同じ。`FreqOffsetHz` (受信の中心からのずれの推定) を足している
- rtlamr v0.9.6 は `-server` を無視して `127.0.0.1:1234` につなぐ。r900rs は `-server` のとおりにつなぐ

## 使い方

rtl_tcp を立ててからつなぐ:

```sh
rtl_tcp -a 127.0.0.1 &
r900rs -server=127.0.0.1:1234 -msgtype=r900 -format=json [-filterid=<ID>,...]
```

| 指定 | 意味 |
|---|---|
| `-server=<host:port>` | rtl_tcp の場所 (既定 `127.0.0.1:1234`) |
| `-filterid=<ID>,...` | この ID だけ出す |
| `-centerfreq=<Hz>` | 中心周波数 (既定 912,380,000) |
| `-symbollength=<n>` | 8, 32, 40, 48, 56, 64, 72, 80, 88, 96 (既定 72)。サンプルレートは 32,768 × n |
| `-tee=<ファイル>` | 受けた IQ をそのまま書く。同じ名前があれば時刻を挟んだ名前で作る |
| `-teemax=<バイト>` | 録音の上限。達したら録音だけやめる |
| `-reserve=<host:port>` | 受けた IQ を rtl_tcp と同じ形で配り直す (ループバックだけ)。rtlamr を並べて動かすのに使う |
| `-decode=false` | 解読しない (配り直し・録音だけ) |
| `-file=<ファイル>` | rtl_tcp の代わりにファイルから読む |
| `-tunergainmode=false` | チューナーの自動ゲイン (AGC) |
| `-tunergain=<dB>` | 固定のゲイン (-30〜60、rtlamr と同じく 0.1 dB に切り捨て)。手動にしてから値を送る |

**ゲインに注意**: ゲインを何も指定しないと、rtlamr と同じく「手動」だけを送る。librtlsdr の R820T はそのときゲインを 0 (一番低い感度) にする (`r820t_set_gain_mode` が `r82xx_set_gain(manual, 0)` を呼ぶ)。受信が弱いときは `-tunergainmode=false` か `-tunergain=<dB>` を付ける。rtlamr で固定のゲインを使うときは `-tunergain` だけを付ける (rtlamr は指定を名前の辞書順に送るので、`-tunergainmode=true` を足すと値のあとに手動に切り替わって 0 に戻る)。

rtlamr のほかのチューナーの指定 (`-agcmode`・`-gainbyindex`・`-freqcorrection` など) は持たず、起動時に断る。

rtl_tcp から 5 秒届かない・切れたときは終了コード 1 で終わる (呼ぶ側が作り直す前提)。

## 確かめ方

```sh
cargo test --release
cargo build --release
# 偽の電波 (偽の ID・積算値の R900 を RS 付きで入れた 8 ビット IQ)
target/release/r900synth -o /tmp/a.iq -seconds=600 -symbollength=72 -seed=1
# 同じファイルを rtlamr と r900rs に読ませて、電文の並びを順まで比べる (ID は出さない)
RTLAMR=/path/to/rtlamr tools/parity.sh /tmp/a.iq 72
```

`r900synth` の指定: `-seconds`・`-symbollength`・`-seed`・`-noise=<0..1>` (1 で電文なし)・`-cw` (一定値)・`-offset=<Hz>`。`r900rs replay <ファイル> [-listen=127.0.0.1:1235]` は、ファイルを rtl_tcp のふりをして 1 回流す。`parity.sh` は rtlamr のために 127.0.0.1:1234 を使う。

`-reserve` と `replay` はループバックだけで待ち受けるが、同じマシンのほかのユーザーや host network のコンテナからはつなげる。

**本物の電波を録音したファイルには、近所のメーターの ID や使用量も入る。** 公開の場所に置かないこと。

## ライセンス

AGPL-3.0-or-later (`LICENSE`)。rtlamr (Copyright (C) 2014 Douglas Hall、AGPL-3.0) から移した部分を含む。
