# Vouch

A local-first, privacy-preserving database of recommendations by you and your trusted friends.

## Overview

Vouch enables you to:
- Create and manage recommendations locally on your device
- Connect with other Vouch users via end-to-end encrypted channels
- Share recommendations with specific contacts or all contacts
- Reshare (revouch) recommendations received from others

Built on three pillars:
1. **Local-first**: Your data lives on your device. The network is for sync, not storage.
2. **Privacy by design**: E2E encryption for all sync. You control what you share.
3. **Trust through relationships**: Recommendations flow through your personal network, not algorithms.

## Architecture

See [ARCHITECTURE.md](./ARCHITECTURE.md) for the full system design.

## Building

Vouch is built with Rust and [GPUI](https://github.com/zed-industries/zed/tree/main/crates/gpui).

```sh
cargo build
cargo run
```

## License

This project is licensed under the GNU General Public License v3.0 - see the [LICENSE](LICENSE) file for details.