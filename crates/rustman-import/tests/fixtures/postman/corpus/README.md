# Script migration corpus

Public Postman collections used to track the script migration conversion
rate (`../../../corpus.rs`, baseline in `baseline.txt`). They are copied
unchanged from the integration tests of Newman
(<https://github.com/postmanlabs/newman/tree/develop/test/integration>,
Apache License 2.0):

| File | Upstream name |
|------|---------------|
| `echo-v2.postman_collection.json` | `echo-v2.postman_collection.json` |
| `head-requests.postman_collection.json` | `head-requests.postman_collection.json` |
| `multi-level-folders-v2.postman_collection.json` | `multi-level-folders-v2.postman_collection.json` |
| `request-chaining-test.postman_collection.json` | `request-chaining-test.postman_collection.json` |
| `set-next-request.postman_collection.json` | `set-next-request.postman_collection.json` |

The tests only parse them; nothing is sent.
