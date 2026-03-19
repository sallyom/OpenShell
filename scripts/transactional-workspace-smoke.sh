#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BIN="${OPENSHELL_SANDBOX_BIN:-$ROOT_DIR/target/debug/openshell-sandbox}"
TEST_ROOT="${OPENSHELL_TX_SMOKE_ROOT:-/tmp/openshell-tx-smoke}"
KEEP_UPPER="${OPENSHELL_TX_KEEP_UPPER:-1}"
BACKEND="${OPENSHELL_TX_BACKEND:-auto}"
LOG_LEVEL="${OPENSHELL_TX_LOG_LEVEL:-debug}"
POLICY_RULES="${OPENSHELL_TX_POLICY_RULES:-$ROOT_DIR/crates/openshell-sandbox/data/sandbox-policy.rego}"
POLICY_DATA="${OPENSHELL_TX_POLICY_DATA:-$ROOT_DIR/crates/openshell-sandbox/testdata/sandbox-policy.yaml}"
POLICY_DATA_EFFECTIVE=""

if [[ "$(uname -s)" != "Linux" ]]; then
  echo "FAIL: this smoke test only runs on Linux" >&2
  exit 1
fi

if [[ ! -x "$BIN" ]]; then
  echo "FAIL: openshell-sandbox binary not found or not executable: $BIN" >&2
  echo "Set OPENSHELL_SANDBOX_BIN or build the crate first." >&2
  exit 1
fi

if [[ ! -f "$POLICY_RULES" ]]; then
  echo "FAIL: policy rules file not found: $POLICY_RULES" >&2
  exit 1
fi

if [[ ! -f "$POLICY_DATA" ]]; then
  echo "FAIL: policy data file not found: $POLICY_DATA" >&2
  exit 1
fi

if ! command -v mount >/dev/null 2>&1; then
  echo "FAIL: mount command not found" >&2
  exit 1
fi

cleanup() {
  if [[ -d "$TEST_ROOT/txroot/merged" ]]; then
    if command -v mountpoint >/dev/null 2>&1 && mountpoint -q "$TEST_ROOT/txroot/merged"; then
      fusermount3 -u "$TEST_ROOT/txroot/merged" >/dev/null 2>&1 || true
      umount -l "$TEST_ROOT/txroot/merged" >/dev/null 2>&1 || true
    fi
  fi
  if [[ -n "$POLICY_DATA_EFFECTIVE" && "$POLICY_DATA_EFFECTIVE" != "$POLICY_DATA" ]]; then
    rm -f "$POLICY_DATA_EFFECTIVE"
  fi
  rm -rf "$TEST_ROOT"
}
trap cleanup EXIT

BASE_DIR="$TEST_ROOT/base"
TX_ROOT="$TEST_ROOT/txroot"
RUN_USER="${OPENSHELL_TX_RUN_USER:-sandbox}"
RUN_GROUP="${OPENSHELL_TX_RUN_GROUP:-sandbox}"

if [[ "$(id -u)" != "0" ]]; then
  RUN_USER="${OPENSHELL_TX_RUN_USER:-$(id -un)}"
  RUN_GROUP="${OPENSHELL_TX_RUN_GROUP:-$(id -gn)}"
fi

mkdir -p "$BASE_DIR"
mkdir -p "$TX_ROOT"
printf 'hello\n' > "$BASE_DIR/file.txt"
chmod 0775 "$BASE_DIR" "$TX_ROOT"
chmod 0664 "$BASE_DIR/file.txt"

if [[ "$(id -u)" == "0" ]]; then
  if getent passwd "$RUN_USER" >/dev/null 2>&1 && getent group "$RUN_GROUP" >/dev/null 2>&1; then
    chown -R "$RUN_USER:$RUN_GROUP" "$TEST_ROOT"
  else
    echo "FAIL: expected runtime user/group $RUN_USER:$RUN_GROUP not found" >&2
    exit 1
  fi
fi

POLICY_DATA_EFFECTIVE="$POLICY_DATA"
if [[ "$RUN_USER" != "sandbox" || "$RUN_GROUP" != "sandbox" ]]; then
  POLICY_DATA_EFFECTIVE="$TEST_ROOT/policy.yaml"
  python3 - "$POLICY_DATA" "$POLICY_DATA_EFFECTIVE" "$RUN_USER" "$RUN_GROUP" <<'PY'
import sys
import yaml

src, dst, user, group = sys.argv[1:]
with open(src, "r", encoding="utf-8") as f:
    data = yaml.safe_load(f)

data.setdefault("process", {})
data["process"]["run_as_user"] = user
data["process"]["run_as_group"] = group
data["network_policies"] = {}
filesystem_policy = data.setdefault("filesystem_policy", {})
filesystem_policy["include_workdir"] = True
filesystem_policy["read_write"] = []

with open(dst, "w", encoding="utf-8") as f:
    yaml.safe_dump(data, f, sort_keys=False)
PY
fi

echo "== transactional workspace smoke test =="
echo "binary: $BIN"
echo "test root: $TEST_ROOT"
echo "backend: $BACKEND"
echo "log level: $LOG_LEVEL"
echo "policy rules: $POLICY_RULES"
echo "policy data: $POLICY_DATA_EFFECTIVE"
echo "run user/group: $RUN_USER:$RUN_GROUP"
echo

set +e
"$BIN" \
  --workdir "$BASE_DIR" \
  --log-level "$LOG_LEVEL" \
  --policy-rules "$POLICY_RULES" \
  --policy-data "$POLICY_DATA_EFFECTIVE" \
  --transactional-workspace-root "$TX_ROOT" \
  --transactional-workspace-backend "$BACKEND" \
  $( [[ "$KEEP_UPPER" == "1" ]] && printf '%s' "--transactional-workspace-keep-upper" ) \
  -- /bin/bash -c '
    echo "sandbox pwd: $PWD"
    test -f file.txt
    echo "changed" > file.txt
    echo "new file" > created.txt
    mkdir -p subdir
    echo "nested" > subdir/nested.txt
    cat file.txt
    ls -la
  '
CMD_STATUS=$?
set -e

if [[ $CMD_STATUS -ne 0 ]]; then
  echo "FAIL: openshell-sandbox command exited with status $CMD_STATUS" >&2
  exit $CMD_STATUS
fi

echo
echo "== verification =="

BASE_CONTENT="$(cat "$BASE_DIR/file.txt")"
if [[ "$BASE_CONTENT" != "hello" ]]; then
  echo "FAIL: base file was modified unexpectedly: $BASE_CONTENT" >&2
  exit 1
fi
echo "PASS: base file remained unchanged"

if [[ "$KEEP_UPPER" == "1" ]]; then
  if [[ ! -f "$TX_ROOT/upper/file.txt" ]]; then
    echo "FAIL: expected upperdir file not found at $TX_ROOT/upper/file.txt" >&2
    exit 1
  fi
  UPPER_CONTENT="$(cat "$TX_ROOT/upper/file.txt")"
  if [[ "$UPPER_CONTENT" != "changed" ]]; then
    echo "FAIL: upperdir file content mismatch: $UPPER_CONTENT" >&2
    exit 1
  fi
  echo "PASS: upperdir captured modified file"

  if [[ ! -f "$TX_ROOT/upper/created.txt" ]]; then
    echo "FAIL: expected created file not found in upperdir" >&2
    exit 1
  fi
  echo "PASS: upperdir captured new file"

  if [[ ! -f "$TX_ROOT/upper/subdir/nested.txt" ]]; then
    echo "FAIL: expected nested file not found in upperdir" >&2
    exit 1
  fi
  echo "PASS: upperdir captured nested file"
else
  if [[ -d "$TX_ROOT/upper" ]]; then
    echo "FAIL: upperdir should have been cleaned up when keep-upper=0" >&2
    exit 1
  fi
  echo "PASS: upperdir cleaned up"
fi

if [[ -d "$TX_ROOT/merged" ]]; then
  echo "FAIL: merged dir should have been removed during cleanup" >&2
  exit 1
fi
echo "PASS: merged dir cleaned up"

if [[ -d "$TX_ROOT/work" ]]; then
  echo "FAIL: work dir should have been removed during cleanup" >&2
  exit 1
fi
echo "PASS: work dir cleaned up"

echo
echo "SUCCESS: transactional workspace smoke test passed"
