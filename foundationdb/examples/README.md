
| Example  | Description |
| ------------- | ------------- |
| atomic-op-counter.rs  | Uses atomic operations to increment and decrement a 64-bit value in the database |
| blob.rs  | Stores large data by splitting it into 1KB chunks |
| blob-with-manifest.rs  | More robust blob storage, where each file has a UUID and metadata that includes an SHA-256 digest |
| class-scheduling.rs  | Based on the Class Scheduling tutorial in the FoundationDB documentation |
| hello-world.rs  | Minimal example that connects to the database and writes and reads a key |
| simple-index.rs  | Creates an index as described in the FoundationDB Data Modeling guide |

See [tests](../tests/) for additional examples
