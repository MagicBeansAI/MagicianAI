# magician-storage-s3

Dormant S3-compatible `ObjectStore` and `DatasetStore` adapters.

Default Magician startup does **not** depend on this crate. Adapters open only
from an explicit `remote_durable` profile (`open_from_profile`) or a hermetic
in-memory S3-semantic backend (`RemoteStores::hermetic`).

- TLS: `http://` endpoints are refused unless `RemoteOpenOptions.allow_http`
- Encryption: `objects.encryption` must be `required`; puts send SSE-S3
- Credentials: `SecretStore` only; `Debug` is redacted
- Integrity: PUT stores `x-amz-meta-blake3` / `x-amz-meta-len`; full GET and
  HEAD require both and compare body digest/len. Extra headers including
  `If-Match` / `If-None-Match` / `Range` are SigV4-signed. Missing ETag is
  not treated as a CAS token.
- Multipart: abort + bounded abandoned-upload cleanup
- Live MinIO/S3 qualification is opt-in (`MAGICIAN_OBJECT_ENDPOINT`), not CI default

```
make test-storage-s3
```
