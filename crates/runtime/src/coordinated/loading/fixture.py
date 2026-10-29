import base64
import hashlib
import json
import os
import sys
import time

mode, binding, log = sys.argv[1:]


def record(kind):
    with open(log, "a", encoding="utf-8") as file:
        file.write(json.dumps({"kind": kind, "pid": os.getpid()}) + "\n")


def emit(value):
    print(json.dumps(value, separators=(",", ":")), flush=True)


def receive(kind):
    value = json.loads(sys.stdin.readline())
    assert value["type"] == kind
    assert "attempt_id" not in value
    record(kind)
    return value


if mode == "stall_ready":
    time.sleep(30)
capabilities = ["context_restore_v2", "context_continue_v2", "fixture_retention", "context_observe_v1"]
if mode.startswith("missing_"):
    capabilities.remove({"missing_restore": capabilities[0],
                         "missing_continue": capabilities[1],
                         "missing_retention": capabilities[2]}[mode])
if mode.startswith("continue_capture"):
    capabilities.extend(["context_capture_v1", "context_export_v1"])
if mode == "continue_capture_steer":
    capabilities.append("message_ack_v2")
ready = {"type": "ready", "version": 1, "capabilities": capabilities}
if mode == "observe_inspect":
    capabilities.remove("context_continue_v2")
if mode == "observe_missing":
    capabilities.remove("context_observe_v1")
emit(ready)
if mode == "duplicate_ready":
    emit(ready)
begin = receive("restore_begin_v2")
if mode == "stall_prepared":
    time.sleep(30)
prepared = {"type": "restore_prepared_v2", "scope": begin["scope"],
            "begin_binding": binding}
if mode == "bad_prepared":
    prepared["begin_binding"] = "00" * 32
if mode == "early_restored":
    prepared = {"type": "context_restored_v2", "scope": begin["scope"],
                "begin_binding": binding, "payload_digest": "00" * 32,
                "receipt": "01234567-89ab-4cde-8f01-23456789abcd"}
if mode == "ordinary":
    prepared = {"type": "text", "attempt_id": "fake", "text": "unadmitted"}
emit(prepared)
if mode == "duplicate_prepared":
    emit(prepared)
payload = receive("restore_payload_v2")
assert payload["payload_digest"] == begin["descriptor"]["payload_digest"]
assert payload["payload_length"] == begin["descriptor"]["payload_length"]
if mode == "stall_restored":
    time.sleep(30)
restored = {"type": "context_restored_v2", "scope": payload["scope"],
            "begin_binding": payload["begin_binding"],
            "payload_digest": payload["payload_digest"], "receipt": payload["receipt"]}
if mode == "bad_binding":
    restored["begin_binding"] = "00" * 32
if mode == "bad_payload":
    restored["payload_digest"] = "00" * 32
if mode == "bad_receipt":
    restored["receipt"] = "01234567-89ab-4cde-8f01-23456789abcd"
if mode == "bad_scope":
    restored["scope"]["restore_id"] = "other"
emit(restored)
if mode == "extra":
    emit(restored)
if mode == "exit":
    sys.exit(0)
if mode.startswith("observe_") or mode.startswith("continue_"):
    observation = receive("context_observe_v1")
    if mode == "observe_stall":
        time.sleep(30)
    body = b"detached-native-observation-fixture"
    response = {"type": "context_observed_v1", "binding": observation["binding"],
                "instance_id": "01234567-89ab-4cde-8f01-23456789abcd",
                "payload_length": len(body), "payload_digest": hashlib.sha256(body).hexdigest(),
                "payload_base64": base64.b64encode(body).decode(), "challenge": observation["challenge"]}
    if mode == "observe_job":
        response["binding"]["job_id"] = "01234567-89ab-4cde-8f01-23456789abcd"
    if mode == "observe_begin":
        response["binding"]["begin_binding"] = "00" * 32
    if mode == "observe_profile":
        response["binding"]["profile_digest"] = "00" * 32
    if mode == "observe_scope":
        response["binding"]["scope"]["restore_id"] = "other"
    if mode == "observe_load":
        response["binding"]["scope"]["load_id"] = "01234567-89ab-4cde-8f01-23456789abcd"
    if mode == "observe_challenge":
        response["challenge"] = "01234567-89ab-4cde-8f01-23456789abcd"
    if mode == "observe_payload":
        response["payload_digest"] = "00" * 32
    if mode.startswith("observe_reject_"):
        response = {"type": "context_observation_rejected_v1", "binding": observation["binding"],
                    "code": mode[len("observe_reject_"):], "challenge": observation["challenge"]}
    if mode == "observe_direction":
        response = observation
    if mode == "observe_ordinary":
        response = {"type": "text", "attempt_id": "fake", "text": "unadmitted"}
    emit(response)
    if mode == "observe_duplicate":
        emit(response)
if mode.startswith("continue_"):
    continuation = receive("continue_context_v2")
    attempt = continuation["continuation"]["attempt_id"]
    accepted = {"type": "continuation_accepted_v2", "scope": continuation["scope"],
                "continuation": continuation["continuation"], "receipt": continuation["receipt"]}
    text = {"type": "text", "attempt_id": attempt, "text": "delta"}
    if mode == "continue_stall":
        time.sleep(30)
    if mode == "continue_before":
        emit(text)
    if mode == "continue_receipt":
        accepted["receipt"] = "01234567-89ab-4cde-8f01-23456789abcd"
    if mode == "continue_identity":
        accepted["continuation"]["input_sequence"] += 1
    emit(accepted)
    if mode == "continue_duplicate":
        emit(accepted)
    if mode == "continue_wrong_attempt":
        text["attempt_id"] = "wrong"
    if mode == "continue_overflow":
        for index in range(66):
            emit(text)
    elif mode == "continue_bytes":
        text["text"] = "x" * 262145
        emit(text)
    elif mode == "continue_capture_steer":
        message = json.loads(sys.stdin.readline())
        record(message["type"])
        assert message["type"] == "message"
        emit({"type": "message_accepted", "attempt_id": attempt,
              "message_id": message["message_id"], "receipt": message["receipt"]})
        time.sleep(30)
    elif mode != "continue_quiet":
        emit(text)
        emit({"type": "tool_call", "attempt_id": attempt, "call_id": "call", "name": "read", "input": {"path": "blocked"}})
        response = json.loads(sys.stdin.readline())
        record(response["type"])
        assert response["type"] == "tool_result"
        emit({"type": "settled", "attempt_id": attempt, "outcome": "action_required", "result": "done"})
if mode.startswith("continue_capture"):
    export = json.loads(sys.stdin.readline())
    record(export["type"])
    assert export["type"] == "context_export"
    if mode == "continue_capture_stall":
        time.sleep(30)
    body = b"native-baseline-plus-new-delta"
    candidate = {"type": "context_candidate", "attempt_id": attempt, "capture_id": export["capture_id"],
                 "format": {"profile": "current", "format_version": "1", "producer_build": "fixture"},
                 "coverage": {"completed_turn_id": "turn", "inputs": [], "results": []},
                 "payload_length": len(body), "payload_digest": hashlib.sha256(body).hexdigest(),
                 "payload_base64": base64.b64encode(body).decode(), "challenge": export["challenge"]}
    if mode == "continue_capture_challenge":
        candidate["challenge"] = "01234567-89ab-4cde-8f01-23456789abcd"
    if mode == "continue_capture_payload":
        candidate["payload_digest"] = "00" * 32
    emit(candidate)
    if mode == "continue_capture_duplicate":
        emit(candidate)
# No duplicate Continue or Start.
line = sys.stdin.readline()
if line:
    record("unexpected")
    sys.exit(2)
