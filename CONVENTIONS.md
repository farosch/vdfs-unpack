# Coding conventions

These rules keep `vdfs-unpack` consistent. Check them before adding code or
reviewing a change. `DEVELOP.md` covers the commands and test fixtures.

## Project layout

- `src/main.rs`: entry point only. It reads the image and dispatches on
  `vdfs1002::is_vdfs1002`, then checks `vdfs4::is_vdfs4` and warns on stderr if
  neither format matches. Without `--force` that is an error; with `--force` it
  is a warning before falling back to VDFS4.
- `src/cli.rs`: the shared `Cli` argument struct.
- `src/lzo1x.rs`: the LZO1X codec, shared by both formats. Format-neutral code
  lives at the top level.
- `src/vdfs4/`: everything specific to VDFS4 (layout 2006 and 2007).
  `mod.rs` holds the pipeline entry point (`run`). The other files are one
  concern each: `disk`, `superblock`, `btree`, `cattree`, `extents`,
  `snapshot`, `reader`, `decompress`, `unpacker`, `manifest`.
- `src/hex.rs`: the shared hex helpers. Do not add another hex helper to a
  module. Use this one.
- `src/vdfs1002/`: everything specific to VDFS1002. `mod.rs` only declares and
  re-exports the modules. `cli.rs` is the CLI handler for that format.
- A new format gets its own directory under `src/`. It must not import from
  another format's module. Shared code moves to the top level.
- No module imports freesung or any other project. This crate is independent.

## Module paths

- Inside a format directory, reach siblings with `super::` and shared code
  with `crate::`. Do not use `crate::<format-module>` paths for code in the
  same format.
- Keep `pub(crate)` for items used across modules. Use `pub` only for items
  that are part of the crate's library-style surface.
- Remove dead code instead of silencing it. Do not add `#![allow(dead_code)]`.

## Errors and robustness

- Return `anyhow::Result`. Use `bail!` for rejected input and `.context(...)`
  to name the failing structure.
- Image data is untrusted. Never panic on it: no `unwrap`, no unchecked
  indexing or arithmetic that can overflow. Use checked or saturating
  arithmetic and bounds checks, and return an error.
- Warnings go to stderr. A bad but recoverable record should warn, not fail
  the whole run, if the existing behaviour does so.

## Style

- Format with `cargo fmt`. Run `cargo clippy --locked --all-targets` and keep
  it at zero warnings.
- Prefer iterators and `for x in &items` over index loops.
- Use `div_ceil` for ceiling division and `let` chains for nested `if let`.
- Comment only code that needs clarification (for example a non-obvious
  on-disk layout or a deliberate deviation). Do not restate what the code
  does.
- Keep functions focused. If a function needs many parameters, group the
  read-only state into a context struct (see `WalkCtx` in
  `src/vdfs4/unpacker.rs`).

## Tests

- Every behaviour change needs a test. Put unit tests in a `#[cfg(test)]`
  module in the same file.
- Tests use synthetic fixtures built in memory or with `mkfs`. Do not commit
  real Samsung images, keys, or extracted firmware data.
- Codec tests check against reference streams (for example liblzo2) where
  they exist.
- The test command is `cargo test --locked`. Update the counts in `DEVELOP.md`
  when they change.

## Documentation

- When code, tests, build steps, or behaviour change, update `DEVELOP.md`
  (layout and commands), `README.md` (user-facing behaviour), and `TODO.md`
  (status and open leads) in the same change.
- Refer to files by their current path. Refer to functions by name, not by
  line number, because line numbers drift.
