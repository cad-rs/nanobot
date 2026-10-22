# nanobot vendor pin (NR7)

nanobot vendors the same three crates as `cad-rs/workbench-rs`:

| crate | path |
|---|---|
| `gpui-ce` | `.vendor/gpui-ce` |
| `gpui-component` | `.vendor/gpui-component` |
| `gpui-rhai` | `.vendor/gpui-rhai` |

**Why a copy at all.** `[patch.crates-io]` only applies from the *workspace root*,
so a workspace that consumes these crates must carry the patch table itself. nanobot
therefore keeps its own copy for standalone builds, and rcad-pro keeps one in
`workbench-rs/.vendor` (its root patch table). Both trees must be the **same
content**, or the same build could end up with two `gpui-ce` crates — types stop
being interchangeable and the wgpu version skew makes zero-copy surface compositing
fail silently (design NR7).

**Pinned from.** `cad-rs/workbench-rs` @ `9ca5b62240c3073b266acdbfcb6908ee7ac5ab19`
(`feat(core): generic crash-recovery probe; fix clean-marker path mismatch`),
copied verbatim on 2026-10-22.

**Keeping them in sync.** `tools/nanobot-vendor-check.py` in rcad-pro asserts the
two trees are byte-identical and that the wgpu version is pinned once. If
workbench-rs's vendor is refreshed, re-copy and re-run that check in the same
commit. If the two ever need to diverge, the escape hatch is a shared
`cad-rs/gpui-kit` vendor repository (design §3.3) — not a second pin.
