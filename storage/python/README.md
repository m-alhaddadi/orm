# orm-storage

Independent async object storage: local files, optional S3-compatible providers,
durable versioned references, bounded streaming and explicit signing/deletion.
No ORM installation is required.

Import `LocalStorage`, `Reference`, and `Registry` from `orm_storage`. For S3, install
the `s3` extra and pass an application-owned aiobotocore client to
`orm_storage.s3.S3Storage`.

The complete usage, wire, stream ownership and recovery contract is maintained in
`storage/README.md` in the source repository. This package uses Python 3.11+.
