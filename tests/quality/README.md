# Deterministic campaign fixtures

[`campaign.json`](campaign.json) pins a campaign for the harness in
[`scripts/quality`](../../scripts/quality/campaign.py): a neutral provider ID and
model, an effort from the contract vocabulary, a list of seeds, a repeat count and
the task fixtures in [`tasks/`](tasks), each pinned by its SHA-256 digest. Every
name and prompt is synthetic, and paths stay under `/workspace/`.

The harness runs each task once per seed and repeat against one of two providers:

- `fake` answers in process from the request digest and opens no socket.
- `local` starts a stub server on `127.0.0.1` with an ephemeral port and calls it
  over the provisional `chat` wire format of
  [`packages/provider-local`](../../packages/provider-local/README.md). The client
  refuses any host that is not a literal loopback address.

A run writes two files. `manifest.json` is the resolved manifest with each task's
digest and the digest of the outcomes. `outcomes.jsonl` holds one record per call
with the task, seed, repeat, request digest, a status of `replied`,
`tool_requested` or `endpoint_failure`, the reply text, any requested tool calls
and the reported token counts. Tool calls are recorded, never run.

```sh
python3 scripts/quality/campaign.py --manifest tests/quality/campaign.json --verify
python3 scripts/quality/campaign.py --manifest tests/quality/campaign.json --out <dir>
python3 -m unittest discover -s tests/quality -v
```

`--verify` runs the campaign twice and fails unless both runs wrote identical
bytes. CI runs it in a non-blocking job.

## Limitations

- Outcomes are recorded without grading. No record scores or compares a reply,
  and the harness makes no claim about model or benchmark equivalence; that stays
  unverified unless decision D46-4 authorizes a live campaign.
- Only the fake and stub providers run. Hosted providers and real local models are
  out of scope, and the stub's replies are a digest of the request, not model
  output.
- The effort is recorded in the manifest but not sent: the `chat` wire format
  carries no effort field. The seed is sent as a provisional `seed` field.
- Wire names are provisional pending the naming decision.
