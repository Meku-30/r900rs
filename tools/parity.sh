#!/usr/bin/env bash
# Copyright (C) 2026 Meku-30
# SPDX-License-Identifier: AGPL-3.0-or-later
# parity.sh <IQ ファイル> [symbollength]
# 同じ IQ ファイルを r900rs replay で rtl_tcp のふりをして流し、rtlamr と r900rs の両方に読ませて、
# 出た電文 (Message の全部の欄) の並びを順まで比べる。Time・Offset・Length・FreqOffsetHz は比べない。
# 一致なら "parity: OK <件数>"、違えば差の件数だけ出して終了コード 1。ID は画面に出さない。
#
# 両方とも同じバイト列を最初から同じ区切り (2 × block_size バイト) で読むので、並びが同じなら
# 同じブロックで同じ電文を出したことになる。replay が流し終えて閉じると、どちらも最後の
# 端数のブロックを捨てて読み取りのエラーで終わる (それは正常として扱う)。
#
# 環境変数: RTLAMR (既定 ~/.local/bin/rtlamr)、R900RS (既定 このクレートの target/release/r900rs)
set -euo pipefail

if [[ $# -lt 1 || $# -gt 2 ]]; then
    echo "使い方: parity.sh <IQ ファイル> [symbollength]" >&2
    exit 2
fi
iq=$1
sl=${2:-72}
here=$(cd "$(dirname "$0")" && pwd)
rtlamr=${RTLAMR:-$HOME/.local/bin/rtlamr}
r900rs=${R900RS:-$here/../target/release/r900rs}
[[ -r $iq ]] || { echo "parity: 読めない: $iq" >&2; exit 2; }
[[ -x $rtlamr && -x $r900rs ]] || { echo "parity: rtlamr か r900rs がない" >&2; exit 2; }

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

# replay で 1 回流し、クライアント ($3...) の標準出力を $1 に書く。$2 は待ち受けのポート
run_one() {
    local out=$1 port=$2
    shift 2
    local rlog=$work/replay.log
    : >"$rlog"
    "$r900rs" replay "$iq" -listen=127.0.0.1:$port 2>"$rlog" &
    local rpid=$!
    for _ in $(seq 100); do
        grep -q listening "$rlog" && break
        kill -0 $rpid 2>/dev/null || break
        sleep 0.05
    done
    if ! grep -q listening "$rlog"; then
        echo "parity: replay が待ち受けられない" >&2
        cat "$rlog" >&2
        return 1
    fi
    # クライアントは流し終えたあと読み取りのエラーで終わる (終了コードは見ない)
    "$@" -server=127.0.0.1:$port >"$out" 2>"$out.err" || true
    # クライアントがつながずに終わると replay は待ち続けるので、10 秒で止める
    for _ in $(seq 200); do
        kill -0 $rpid 2>/dev/null || break
        sleep 0.05
    done
    kill $rpid 2>/dev/null || true
    if ! wait $rpid; then
        # 流し終える前にクライアントが落ちた (つながなかった)
        echo "parity: replay が最後まで流せなかった ($1)" >&2
        grep -v '"ID"' "$rlog" "$out.err" | tail -5 >&2
        return 1
    fi
}

# rtlamr v0.9.6 は -server を無視して必ず 127.0.0.1:1234 につなぐ (main.go が rcvr.Connect(nil))。
# なので rtlamr の回は 1234 で待ち受ける (rtl_tcp などが 1234 を使っていると replay が bind できずに止まる)
run_one "$work/rtlamr.json" 1234 "$rtlamr" -msgtype=r900 -format=json -symbollength="$sl"
run_one "$work/r900rs.json" $((20000 + RANDOM % 20000)) "$r900rs" -msgtype=r900 -format=json -symbollength="$sl"

python3 -I - "$work/rtlamr.json" "$work/r900rs.json" <<'EOF'
import difflib, json, sys

KEYS = ("ID", "Unkn1", "NoUse", "BackFlow", "Consumption", "Unkn3", "Leak", "LeakNow")

def load(path):
    out = []
    with open(path) as f:
        for line in f:
            line = line.strip()
            if not line:
                continue
            m = json.loads(line)["Message"]
            out.append(tuple(m[k] for k in KEYS))
    return out

a = load(sys.argv[1])
b = load(sys.argv[2])
if a == b:
    print(f"parity: OK {len(a)}")
    sys.exit(0)
only_a = only_b = 0
for op, i1, i2, j1, j2 in difflib.SequenceMatcher(a=a, b=b, autojunk=False).get_opcodes():
    if op != "equal":
        only_a += i2 - i1
        only_b += j2 - j1
print(f"parity: NG rtlamr={len(a)} r900rs={len(b)} rtlamrだけ={only_a} r900rsだけ={only_b}")
sys.exit(1)
EOF
