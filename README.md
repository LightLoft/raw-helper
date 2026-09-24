# LightLoft raw helper

LightLoft decodes raw photo files in a separate, sandboxed process: a malformed or hostile
file can at worst stop this helper, never the application. This repository holds that helper
and the protocol the application uses to talk to it.

| Directory   | Crate               | Licence                  | Role |
|-------------|---------------------|--------------------------|------|
| `protocol/` | `loft-raw-protocol` | MIT OR Apache-2.0        | Messages, framing, descriptor passing, shared memory |
| `helper/`   | `loft-raw-helper`   | LGPL-2.1-only            | The helper process, built on [rawler](https://github.com/dnglab/dnglab) (LGPL-2.1) |
| `fuzz/`     | `loft-raw-fuzz`     | LGPL-2.1-only            | Fuzz targets (nightly toolchain) |

## How it works

- The helper's standard input is a Unix stream socket to the application.
- It never opens a path: the application opens each file and passes the descriptor
  (`SCM_RIGHTS`). The helper needs no access to the user's files or to the network.
- Replies carry metadata; pixels come back in shared memory created by the helper, whose
  descriptor travels with the reply. The receiver checks the real size before mapping it.
- Decoder panics are caught and answered as errors; a crash or a hang only costs the file being
  read: the application restarts the helper.
- On macOS the helper confines itself with a Seatbelt profile (`helper/src/system/sandbox.sb`)
  before reading any request: no network, no file writes, no access to the user's files; it
  refuses to run if it cannot. A `SandboxCheck` request lets the application verify it.
- Any single allocation above 2 GiB is refused (`helper/src/alloc.rs`): a file declaring huge
  dimensions stops the helper instead of exhausting the machine's memory.
- Files rawler cannot read fall back to the system decoders (ImageIO and Core Image on macOS),
  inside the same sandboxed process.

## Fuzzing

`fuzz/` holds three cargo-fuzz targets: `decode` (untrusted files through the decoding steps),
`request` (the helper's side of the protocol) and `reply` (the application's side: a compromised
helper must not be able to crash it).

```sh
cargo +nightly fuzz run --fuzz-dir fuzz decode
```

## Building

```sh
cargo build --release --manifest-path helper/Cargo.toml
```

The helper links rawler `0.8.0` with our fuzzing fixes, from the public fork
[LightLoft/dnglab](https://github.com/LightLoft/dnglab) (tag `v0.8.0-lightloft.1`, pinned by
revision in `helper/Cargo.toml`). To use a modified rawler,
point that dependency to your copy (for instance with `[patch.crates-io]`) and rebuild; the
resulting `loft-raw-helper` binary replaces the one shipped with LightLoft.

## Licence

`loft-raw-helper` is distributed under the GNU Lesser General Public License version 2.1
(`helper/LICENSE`), like rawler. `loft-raw-protocol` is available under the MIT licence
(`protocol/LICENSE-MIT`) or the Apache License 2.0 (`protocol/LICENSE-APACHE`), at your option.
LightLoft itself is a separate program and is not covered by these licences.
