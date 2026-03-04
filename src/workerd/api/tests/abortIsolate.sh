#!/bin/bash
# Call abortIsolate() and check for:
#  * exit code nonzero
#  * fatal log line present.
#  * "signal #6: Aborted" message present (skip on Windows).

WORKERD=$1
CONFIG=$2
JSFILE=$3
TEST_TMPDIR="${TEST_TMPDIR:-/tmp}"

cp "$JSFILE" "$TEST_TMPDIR"

# Prepare the config with a given topLevelAbort value and run workerd.
# Sets the global `output` and `exit_code` variables.
run_test() {
  local top_level_abort=$1
  cp "$CONFIG" "$TEST_TMPDIR"
  sed "s/%topLevelAbort/$top_level_abort/" "$TEST_TMPDIR/abortIsolate.wd-test" \
    > "$TEST_TMPDIR/abortIsolate.wd-test.tmp" \
    && mv "$TEST_TMPDIR/abortIsolate.wd-test.tmp" "$TEST_TMPDIR/abortIsolate.wd-test"

  output=$("$WORKERD" test "$TEST_TMPDIR/abortIsolate.wd-test" --experimental --compat-date=2000-01-01 -dTEST_TMPDIR="$TEST_TMPDIR" 2>&1)
  exit_code=$?

  echo "--- captured output ---" >&2
  echo "$output" >&2
  echo "--- end captured output ---" >&2
  echo "" >&2
}

# Assert that $output contains the given string; set failed=1 on mismatch.
expect_in_output() {
  local msg=$1
  if ! echo "$output" | grep -qF "$msg"; then
    echo "FAIL: expected log line not found: $msg" >&2
    failed=1
  fi
}

failed=0

echo "Test 1: topLevelAbort = false"
run_test false

if [ "$exit_code" -eq 0 ]; then
  echo "FAIL: expected nonzero exit code" >&2
  exit 1
fi

expect_in_output "fatal: abortIsolate() called, terminating process; reason = test reason"
expect_in_output "*** Received signal #6: Abort"

echo "Test 2: topLevelAbort = true"
run_test true

if [ "$exit_code" -eq 0 ]; then
  echo "FAIL: expected nonzero exit code" >&2
  exit 1
fi

expect_in_output "NOSENTRY abortIsolate() called at top level, terminating execution; reason = Abort at top level"

if [ "$failed" -ne 0 ]; then
  exit 1
fi

echo "Success"
exit 0
