# nanobot vendor pin (NR7)

The gpui stack is gpui-fast (`longbridge/gpui-fast`) from git plus
`gpui-component` 0.9 from crates.io — neither is vendored. What is left here are
the two things that make the graph resolve to **one** gpui core:

| path | what it is |
|---|---|
| `.vendor/gpui-pre-compat` | four re-export shims (`gpui-pre`, `-macros`, `-platform`, `-sum-tree`) at version 0.3.11, each forwarding to the gpui-fast crate of the same role. `gpui-component` 0.9 and `gpui-rhai` name GPUI through these snapshot packages; the shims make that the workspace's own gpui-fast. |
| `.vendor/gpui-rhai` | `gpui-rhai` 0.4.2 as published, with three local changes listed at the top of its `Cargo.toml` (the `gpui-pre` pin moved to =0.3.11, gpui-fast's keystroke API, wasm32 fixes). |

**Why a copy at all.** `[patch.crates-io]` only applies from the *workspace root*,
so a workspace that consumes these crates must carry the patch table itself.
nanobot keeps its own copy for standalone builds; a product workspace that
consumes both nanobot and workbench-rs points its root patch table at one of the
two. Both trees must be the **same content**, and every gpui-fast `rev` (root
`Cargo.toml`, the shims) the **same commit**, or the build ends up with two gpui
cores whose types don't mix (design NR7).

**Pinned from.** `cad-rs/workbench-rs` @ `6a585c2c362fb05ec63a496d9fbdac18d9fd7240`
(branch `feat/gpui-fast`), copied verbatim on 2026-11-10. gpui-fast is pinned at
`a10f393d8033c5f52f34821d6a25cddadc6174b4`, wgpu at the revision gpui-fast itself
pins (`cddb2f5d80dc0009b2ef6767635140cacf90a007`).

**Keeping them in sync.** If workbench-rs moves its gpui-fast `rev` or refreshes
`.vendor`, re-copy both directories and move the `rev`s in the root `Cargo.toml`
in the same commit. If the two ever need to diverge, the escape hatch is a
shared `cad-rs/gpui-kit` vendor repository (design §3.3) — not a second pin.

**When this can go away.** Once gpui-fast's own compat crates carry the snapshot
`gpui-component` pins, and a `gpui-rhai` release pins that snapshot, matches the
keystroke API and builds for wasm, both directories and their patch entries can
be dropped.
