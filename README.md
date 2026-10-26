# nanobot

**A framework for building agent-native design harnesses.**

nanobot owns the harness *runtime*: the turn/step loop, the event-sourced session,
the guarded tool pipeline, plan state machine, approval policy, LLM adapter seam,
skill runner, assembly, and the gpui workbench with scripted plugins and a wgpu
render slot. A harness product supplies the domain engine, its tool set, its
project container and its skills — and gets a runnable harness.

> Design and migration path: `docs/design.md` in this repository (migrated
> from rcad-pro's `docs/nanobot-architecture.md`). Extracted from the ocad
> design harness; rcad-pro consumes this repository as a git submodule.

## Status

Extraction is incremental. See the migration path in the design doc:

| crate | state |
|---|---|
| `nanobot-cordis` | **landed** — plugin kernel (moved from `ocad-cordis`, zero deps) |
| `nanobot-llm` | **landed** — LLM adapter seam (moved from `ocad-llm`) |
| `nanobot-session` | **landed** — event log + seq + projection + JSONL + fork; base vocabulary + `Extension` for product events; `hydrate()` for build-time log recovery |
| `nanobot-tools` | **landed** — ToolRegistry + three waterfalls + ApprovalPolicy + generalized ToolContract |
| `nanobot-agent` | **landed** — turn/step loop, plan state machine, PlanVerifier seam |
| `nanobot-skills` | **landed** — skill runner (manifest -> listeners), ships no skills |
| `nanobot-boot` | **landed** — `Harness`/`HarnessBuilder` assembly, `harness.toml` identity, CLI modes (`--selfcheck` / `--selfcheck-crash` / `--agent-smoke` / `--agent-run` / `--smoke`), probe-style selfcheck with the three-phase crash-recovery probe, generic project container (`ProjectPaths` / `Manifest` / `Project`) |
| `nanobot-gpu` | **landed** — the wgpu↔gpui render slot: process-wide shared device (`shared_gpu`), offscreen targets, zero-copy compositing (`paint_surface`), and the `ViewportSlot` contract the product's renderer implements |
| `nanobot-rhai` | **landed** — Rhai script plugin host: `plugin.toml` manifest plus an entry point that declares panels / views / commands / toolbar items; loading is isolated, so a bad plugin never stops the others |
| `nanobot-ui` | **landed** — gpui workbench shell: session list, session workspace (conversation + plan review + approval / clarify bars + composer), viewport region, and the panel dock that hosts product and script-plugin panels. Generic over `SessionView` |

## Layout

```
crates/
  nanobot-cordis/   plugin kernel: Context / Events / Fiber / Disposable
  nanobot-llm/      LlmAdapter seam + StreamChunk/BlockAssembler + adapters
  nanobot-session/  event log + seq + projection + JSONL + fork
  nanobot-tools/    ToolRegistry + waterfalls + ApprovalPolicy
  nanobot-agent/    turn/step loop + plan state machine
  nanobot-skills/   skill runner (manifest -> listeners)
  nanobot-boot/     Harness assembly + harness.toml + CLI/selfcheck + project container
  nanobot-gpu/      render slot: shared device + offscreen target + zero copy
  nanobot-rhai/     Rhai script plugin host (manifest -> panels/views/commands)
  nanobot-ui/       gpui workbench shell + panel dock
.vendor/            gpui-ce / gpui-component / gpui-rhai (see .vendor/README.md)
```

The kernel crates are **GUI-free**: no `gpui`, no `wgpu`, no `rcad-*`, no `ocad-*`.
That is enforced by `tools/nanobot-vendor-check.py` in rcad-pro (design §3.2),
and it is what lets the kernel serve a CLI, a test, or an MCP server without
dragging a UI along. The shell layer (`nanobot-gpu` / `nanobot-rhai` /
`nanobot-ui`) is the only place gpui and wgpu appear.

The vendored gpui stack in `.vendor/` is a copy of `cad-rs/workbench-rs`'s, kept
byte-identical so a single `gpui-ce` serves both workspaces — see
`.vendor/README.md`. nanobot's own `[patch.crates-io]` table supplies the vendor
path for standalone builds; a consuming workspace supplies its own (rcad-pro
points at `workbench-rs/.vendor`), which is why the dependency entries in this
workspace carry a version rather than a path.

## Develop

```bash
cargo check --workspace
cargo test  --workspace
```

## License

**The framework is open source: every crate is MIT** (see [LICENSE](LICENSE)).
The extracted crates used to carry the upstream `Proprietary` value; the
framework's own licensing is now settled — it is the open half of an
open-core model.

**The closed half is plugins.** A harness vendor's plugin packages (script
plugins, skill packs, compiled modules) are their own commercial property and
carry their own license; the framework neither requires a plugin to be open
nor inspects its contents beyond the manifest's declared permissions. That
boundary is what makes the model work: anyone can build and ship a design
harness on the framework, and the proprietary value lives in the plugin
packages a vendor sells.

Vendored third-party code under `.vendor/` keeps its own licenses (the gpui
stack is Apache-2.0 / MIT — both permissive and compatible with MIT); see
[.vendor/README.md](.vendor/README.md). Nothing in the vendor tree is covered
by this license.

### Writing a plugin

A plugin is a directory with a `plugin.toml` manifest plus an entry script
(see `crates/nanobot-rhai/src/manifest.rs` for the fields and
`plugins/`-style layout). **A plugin is your property**: ship it under any
license you like, including a closed one. The framework reads only the
manifest's declared `permissions` (an audit declaration, not a sandbox) and
keeps the rest opaque. Keep that boundary in mind when you author one —
whatever you put in the package is yours to license.
