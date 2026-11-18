# AWS SigV4 test suite

The header-signing cases of the AWS SigV4 signing test suite, copied from the
`aws-signing-test-suite/v4` folder of the `aws-sigv4` 1.6.6 crate, which takes
them from the [CRT test suite](https://github.com/awslabs/aws-c-auth/tree/v0.9.0/tests/aws-signing-test-suite)
(Apache-2.0). The presigned-URL (`query-*`) files are left out: Rustman signs
headers only.

Each case holds `context.json` (credentials, region, service, time),
`request.txt`, and the expected `header-canonical-request.txt`,
`header-string-to-sign.txt`, `header-signature.txt` and
`header-signed-request.txt`. `src/sigv4/suite.rs` runs every case.
