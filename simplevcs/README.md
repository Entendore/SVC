# SimpleVCS

A distributed, branchless, production-grade Version Control System written in Rust.

## Features

- **Branchless Architecture**: Single linear history with a movable `HEAD` pointer.
- **Content-Addressed ODB**: All data is SHA-256 hashed and zstd compressed.
- **Sled DB Backend**: Fast, embedded key-value store for indexes and refs.
- **Monorepo Scale**: O(1) index operations using single-blob serialization.
- **RAII File Locks**: Deadlock-free exclusive repository locking via `fs2`.
- **Gitignore Semantics**: Uses the `ignore` crate for fast, standard `.simplevcsignore` handling.
- **Tags & History**: Full tag support and DAG-walking history commands.
- **Crash Recovery**: Write-Ahead Logging (WAL) ensures commits are atomic.
- **Garbage Collection**: `gc` command cleans orphaned objects and compacts the DB.
- **Filesystem Check**: `fsck` verifies SHA-256 integrity of all objects on disk.
- **Cryptographic Signing**: Ed25519 commit signing (`-S`) and verification.
- **Air-Gapped Bundles**: `bundle create` and `unbundle` for offline transfers.
- **TLS Server**: Native HTTPS support via `rustls`.
- **Multi-User Auth**: Server supports multiple user tokens for team access.
- **Server Hooks**: `pre-receive` and `post-receive` hooks for CI/CD enforcement.
- **LFS Streaming**: Large files are streamed via `/lfs/:oid` to prevent OOM.
- **Shallow Clones**: `--depth N` truncates history for fast initial setup.
- **Video Rendering**: `render` command generates MP4s of code history with FFmpeg.
- **Typewriter TUI**: `typewriter` command plays back history in the terminal.

## Getting Started

```bash
# Build the project
cargo build --release

# Initialize a repo
./target/release/simplevcs init my-repo
cd my-repo

# Add and commit
echo "Hello" > README.md
./target/release/simplevcs add .
./target/release/simplevcs commit -m "Initial commit" -a "Your Name"

# View history
./target/release/simplevcs history
