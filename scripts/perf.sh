#!/bin/sh
# Release-build performance measurements. See docs/foundation-implementation.md.
# Usage: sh scripts/perf.sh [output-directory] [scenario-seconds] [launches]
# Uses temporary data directories; application data is never touched.
set -eu
out="${1:-target/perf}"
seconds="${2:-600}"
launches="${3:-10}"
binary="${NIGHTSHIFT_BINARY:-target/release/nightshift}"
mkdir -p "$out"
[ -x "$binary" ] || cargo build --locked --release

now_ms() { python3 -c 'import time; print(int(time.time() * 1000))'; }

echo "Startup: $launches launches"
data="$(mktemp -d)"
i=1
while [ "$i" -le "$launches" ]; do
  launched="$(now_ms)"
  NIGHTSHIFT_DATA_DIR="$data" NIGHTSHIFT_STARTUP_REPORT="$out/startup-$i.json" "$binary"
  python3 - "$out/startup-$i.json" "$launched" <<'PY'
import json, sys
path, launched = sys.argv[1], int(sys.argv[2])
report = json.load(open(path))
report["exec_to_first_frame_ms"] = report["first_frame_unix_ms"] - launched
json.dump(report, open(path, "w"))
print(f"  launch: exec→first frame {report['exec_to_first_frame_ms']} ms, main→first frame {report['main_to_first_frame_ms']} ms")
PY
  i=$((i + 1))
done
rm -rf "$data"

echo "Platform baseline: 30 seconds"
NIGHTSHIFT_PERF_BASELINE=1 NIGHTSHIFT_PERF_SECONDS=30 NIGHTSHIFT_PERF_REPORT="$out/baseline.json" "$binary"

echo "Scenario: $seconds seconds"
NIGHTSHIFT_PERF_SECONDS="$seconds" NIGHTSHIFT_PERF_REPORT="$out/scenario.json" "$binary"

python3 - "$out" "$launches" <<'PY'
import json, statistics, sys
out, launches = sys.argv[1], int(sys.argv[2])
starts = [json.load(open(f"{out}/startup-{i}.json")) for i in range(1, launches + 1)]
def summary(key):
    values = sorted(s[key] for s in starts)
    return {"first": values and starts[0][key], "median": statistics.median(values), "max": values[-1]}
result = {
    "startup": {
        "exec_to_first_frame_ms": summary("exec_to_first_frame_ms"),
        "main_to_first_frame_ms": summary("main_to_first_frame_ms"),
        "main_to_projects_loaded_ms": summary("main_to_projects_loaded_ms"),
    },
    "baseline": json.load(open(f"{out}/baseline.json")),
    "scenario": json.load(open(f"{out}/scenario.json")),
}
for name in ("baseline", "scenario"):
    result[name]["memory"]["samples"] = len(result[name]["memory"]["samples"])
json.dump(result, open(f"{out}/summary.json", "w"), indent=1)
print(json.dumps(result, indent=1))
PY
