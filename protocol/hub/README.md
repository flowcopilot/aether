# Hub protocol data

The values of the hub protocol, as data. `server/PROTOCOL.md` explains the
protocol; these files hold the exact values. The hub in `server/` and the
node in `crates/aether` keep copies of the values that they use, and their
tests check the copies against these files.

## `contract.json`

- `protocol`: the protocol version that a node sends in `x-aether-protocol`.
- `headers`: the request header names.
- `signing`: the clock skew that the hub accepts, the endpoint ID format,
  and the signed messages. Each message is its list of lines joined with
  `\n`, with no final newline. A `{field}` line is the value of that field
  from a vector (integers as decimal text). A `...Sha256` field is the
  lowercase hex SHA-256 of the bytes.
- `errors`: each error code with its HTTP status. An error body is
  `{"error": "<code>"}`.
- `names`, `limits`, `values`: the name rule, the limits and the enum values.
  A pattern matches the whole value.
- `objects`: the keys of each JSON object. `contactKeys` are present only
  when the row is a contact of the node that asks.
- `routes`: each route with its method, path, `auth` (`none`, `signed`, or
  `account` when the node needs a name), body keys, success `status`
  (default 200) and the reply object.

## `vectors.json`

Golden signing vectors. `keys` holds two test-only Ed25519 keys: `seed` is
the 32-byte secret seed (RFC 8032) and `endpoint` the public key, both as
lowercase hex. Never use them outside tests.

`listingValidation` is a list of listing JSON bodies (UTF-8 bytes as a string)
with a `handle` and `valid` flag. Each side replays them through its listing
acceptance rules (the node, the test hub, and the Elixir hub).

Each of `request`, `listing`, `ownerClaim` and `ownerConfirm` is a list of:

```json
{"name": "…", "signer": "personal", "fields": {…}, "message": "…", "signature": "…"}
```

`message` is the exact text that the key signs, built from `fields` with the
message of the same name in `contract.json`. `signature` is the Ed25519
signature of `message` by the `signer` key, as unpadded base64url. Ed25519
signatures are deterministic, so an implementation can sign `message` with
the seed and compare.

In `fields`, `request` also has `body` (the body text) and `listing` has
`listingBytes` (the listing bytes as UTF-8 text) and `listing` (their
unpadded base64url form, as sent in `PUT /v1/listing`).
