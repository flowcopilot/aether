# Control API

The control service uses HTTPS and JSON. All paths start with `/v1`.

`POST /v1/records` publishes or renews one signed record. The request body is the
record JSON. The service verifies the record signature before it stores the
record. It returns `201` for a new record and `200` for a renewal.

`GET /v1/records/{agentId}` resolves one active record. It returns `200` with
the record, `404` when no record exists, and `410` when the stored record has
expired.

`GET /v1/records?capability={capability}` returns an array of active records
that contain the capability.

`DELETE /v1/records/{agentId}` revokes a record. The request body is a signed
record for the same agent. The service verifies the signature and returns
`204`. It returns `404` when no record exists.

The first test uses a self-signed test record. Production nodes accept a record
only when its signing key is pinned by a direct contact or trusted by the
configured directory or managed network.

For a local test, run `aether control test-record > record.json`. This command
creates a signed record for the `test` capability. It is for tests only.
