// Copyright (c) 2017-2022 Cloudflare, Inc.
// Licensed under the Apache 2.0 license found in the LICENSE file or at:
//     https://opensource.org/licenses/Apache-2.0

#include "exception.h"

#include <kj/test.h>

namespace workerd::jsg {
namespace {

// ---------------------------------------------------------------------------
// isExceptionJsgError
// ---------------------------------------------------------------------------

KJ_TEST("isExceptionJsgError: plain jsg.Error") {
  KJ_EXPECT(isExceptionJsgError("jsg.Error: something went wrong"_kj));
}

KJ_TEST("isExceptionJsgError: broken.outputGateBroken; jsg.Error (STOR-5119)") {
  KJ_EXPECT(isExceptionJsgError("broken.outputGateBroken; jsg.Error: Abort reason"_kj));
}

KJ_TEST("isExceptionJsgError: broken.exceededCpu; jsg.Error") {
  KJ_EXPECT(isExceptionJsgError("broken.exceededCpu; jsg.Error: CPU limit exceeded"_kj));
}

KJ_TEST("isExceptionJsgError: broken.exceededMemory; jsg.Error") {
  KJ_EXPECT(isExceptionJsgError("broken.exceededMemory; jsg.Error: memory limit exceeded"_kj));
}

KJ_TEST("isExceptionJsgError: overload queue message (plain jsg.Error, output gate also broken)") {
  KJ_EXPECT(isExceptionJsgError(
      "jsg.Error: Durable Object is overloaded. Requests queued for too long."_kj));
}

KJ_TEST("isExceptionJsgError: remote. prefix is stripped") {
  KJ_EXPECT(isExceptionJsgError("remote.broken.outputGateBroken; jsg.Error: Abort reason"_kj));
}

KJ_TEST("isExceptionJsgError: multiple remote. prefixes are stripped") {
  KJ_EXPECT(
      isExceptionJsgError("remote.remote.broken.outputGateBroken; jsg.Error: Abort reason"_kj));
}

KJ_TEST("isExceptionJsgError: broken.inputGateBroken; jsg.Error is not matched") {
  // inputGateBroken is handled separately by isExceptionFromInputGateBroken.
  KJ_EXPECT(!isExceptionJsgError("broken.inputGateBroken; jsg.Error: DO reset after cleanup"_kj));
}

KJ_TEST("isExceptionJsgError: broken.updated; jsg.Error is not matched") {
  KJ_EXPECT(!isExceptionJsgError(
      "broken.updated; jsg.Error: Durable Object reset because its code was updated."_kj));
}

KJ_TEST("isExceptionJsgError: broken.dropped; jsg.Error is not matched") {
  KJ_EXPECT(!isExceptionJsgError(
      "broken.dropped; jsg.Error: Actor exceeded event execution time and was disconnected."_kj));
}

KJ_TEST("isExceptionJsgError: worker_do_not_log tag is not matched") {
  // broken.dropped; worker_do_not_log; jsg.Error comes from the alarm timeout path. It does not
  // match any of the specific broken prefixes we check for, so it correctly returns false.
  KJ_EXPECT(!isExceptionJsgError(
      "broken.dropped; worker_do_not_log; jsg.Error: Alarm exceeded its allowed execution time"_kj));
}

KJ_TEST("isExceptionJsgError: jsg.TypeError is not matched") {
  KJ_EXPECT(!isExceptionJsgError("jsg.TypeError: bad type"_kj));
}

KJ_TEST("isExceptionJsgError: jsg.RangeError is not matched") {
  KJ_EXPECT(!isExceptionJsgError("jsg.RangeError: out of range"_kj));
}

KJ_TEST("isExceptionJsgError: jsg.DOMException is not matched") {
  KJ_EXPECT(!isExceptionJsgError("jsg.DOMException(OperationError): op failed"_kj));
}

KJ_TEST("isExceptionJsgError: jsg-internal errors are not matched") {
  KJ_EXPECT(!isExceptionJsgError("jsg-internal.Error: internal detail"_kj));
}

KJ_TEST("isExceptionJsgError: broken + jsg-internal is not matched") {
  KJ_EXPECT(!isExceptionJsgError("broken.outputGateBroken; jsg-internal.Error: storage reset"_kj));
}

KJ_TEST("isExceptionJsgError: internal storage write error is not matched") {
  // actor-cache.c++ uses jsg-internal.Error for internal platform failures so they are not
  // misclassified as user errors when deciding alarm retry limits.
  KJ_EXPECT(
      !isExceptionJsgError("broken.outputGateBroken; jsg-internal.Error: Internal error in Durable "
                           "Object storage write caused object to be reset."_kj));
}

KJ_TEST("isExceptionJsgError: plain internal C++ exception is not matched") {
  KJ_EXPECT(!isExceptionJsgError("OVERLOADED: storage broke"_kj));
}

// ---------------------------------------------------------------------------
// isExceptionFromInputGateBroken
// ---------------------------------------------------------------------------

KJ_TEST("isExceptionFromInputGateBroken: basic match") {
  KJ_EXPECT(isExceptionFromInputGateBroken(
      "broken.inputGateBroken; jsg.Error: DO reset after cleanup"_kj));
}

KJ_TEST("isExceptionFromInputGateBroken: KJ rpc remote exception prefix is stripped") {
  // stripRemoteExceptionPrefix() strips "remote exception: " (KJ RPC tunneling format).
  // Note: the "remote." prefix from annotateBroken() is NOT stripped by this function.
  KJ_EXPECT(isExceptionFromInputGateBroken(
      "remote exception: broken.inputGateBroken; jsg.Error: DO reset after cleanup"_kj));
}

KJ_TEST("isExceptionFromInputGateBroken: annotateBroken remote. prefix is not stripped") {
  // annotateBroken() prepends "remote." which stripRemoteExceptionPrefix() does not handle
  // (it only strips "remote exception: "). isExceptionJsgError strips both forms.
  KJ_EXPECT(!isExceptionFromInputGateBroken(
      "remote.broken.inputGateBroken; jsg.Error: DO reset after cleanup"_kj));
}

KJ_TEST("isExceptionFromInputGateBroken: outputGateBroken is not matched") {
  KJ_EXPECT(!isExceptionFromInputGateBroken("broken.outputGateBroken; jsg.Error: Abort reason"_kj));
}

KJ_TEST("isExceptionFromInputGateBroken: plain jsg.Error is not matched") {
  KJ_EXPECT(!isExceptionFromInputGateBroken("jsg.Error: something"_kj));
}

}  // namespace
}  // namespace workerd::jsg
