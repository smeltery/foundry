# Catalog contract fixtures

`example.json` is an independently authored synthetic record. Its `.example`
URLs are reserved placeholders, not working services. No connector inventory,
provider descriptions, assets, or third-party schemas are included.

Run the offline checks from the repository root:

```sh
python3 scripts/checks/catalog.py
python3 -m unittest discover -s tests/catalog -v
```

Pass explicit catalog document paths to check real inputs. With no arguments,
the checker validates only the synthetic fixture; it does not discover or
certify a production catalog.

The JSON Schema defines document shapes. The stdlib checker additionally enforces
unique connector/server/skill IDs within their respective scopes, safe relative
icon paths, credential-free HTTP(S) URLs, case-insensitive header uniqueness,
and API-key placeholders. API-key headers require `${credential}`; OAuth and
unauthenticated records cannot use that placeholder. Custom static headers are
supported. Adapter endpoints must explicitly remain `unavailable`.

The checker implements only the schema keywords currently used in
`marketplace/schema.json`, failing on unknown keywords. It is not a complete
JSON Schema validator. `relative-path` is a custom format; other validators
must register it and run the cross-field checks separately. Schema-only
validation does not enforce ID uniqueness or header/auth relationships.

Validation does not check endpoint reachability, actual MCP transport support,
OAuth discovery or scopes, credentials, adapter implementations, asset existence,
symlink resolution, skill downloads, revision existence/content integrity,
licensing, or end-to-end client installation. Skill pins must be lowercase
40- or 64-character hexadecimal revisions; their syntax alone does not establish
that remote content is immutable. Runtime code must resolve local assets within
a trusted root and supply credentials from separate connection state. No
runtime fetching, authentication, secret storage, or connector loading is
implemented here.
