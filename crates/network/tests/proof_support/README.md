# Public proof fixtures

These synthetic public RSA keys and precomputed RS256 signatures test a local
fixture issuer. They grant no access to any service. Private signing keys and
endpoint seed material were generated transiently and deleted after fixture
creation; no private keys are stored here.

Claims use a fixed synthetic clock. Cases deliberately include malformed headers,
invalid claims and an untrusted signer's signature. The whitespace vector signs
its exact formatted JSON bytes to catch accidental JSON reserialization.

The second key identifier intentionally references the same public key to test
explicit key selection and additive policy configuration. These fixtures are
original test data, not deployment credentials or copied external vectors.
