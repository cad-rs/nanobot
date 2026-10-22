# nanobot

**A framework for building agent-native design harnesses.**

nanobot owns the harness *runtime*: the turn/step loop, the event-sourced session,
the guarded tool pipeline, plan state machine, approval policy, LLM adapter seam,
skill runner, assembly, and the gpui workbench with scripted plugins and a wgpu
render slot. A harness product supplies the domain engine, its tool set, its
project container and its skills — and gets a runnable harness.

> Design and migration path: `docs/nanobot-architecture.md` in
> [rcad-pro](https://github.com/cad-rs/rcad-pro). Extracted from the ocad
> design harness; rcad-pro consumes this repository as a git submodule.

## Status

Extraction is incremental. See the migration path in the architecture doc:

| crate | state |
|---|---|
| `nanobot-cordis` | **landed** — plugin kernel (moved from `ocad-cordis`, zero deps) |
| `nanobot-llm` | **landed** — LLM adapter seam (moved from `ocad-llm`) |
| `nanobot-session` | **landed** — event log + seq + projection + JSONL + fork; base vocabulary + `Extension` for product events |
| `nanobot-tools` | **landed** — ToolRegistry + three waterfalls + ApprovalPolicy + generalized ToolContract |
| `nanobot-agent` | **landed** — turn/step loop, plan state machine, PlanVerifier seam |
| `nanobot-skills` | **landed** — skill runner (manifest -> listeners), ships no skills |
| `nanobot-boot` | pending — `Harness` assembly + CLI + selfcheck + project container |
| `nanobot-gpu` | pending — wgpu↔gpui render slot (mechanism out of `ocad-viewport`) |
| `nanobot-rhai` | pending — Rhai script plugin host |
| `nanobot-ui` | pending — gpui workbench shell |

## Layout

```
crates/
  nanobot-cordis/   plugin kernel: Context / Events / Fiber / Disposable
  nanobot-llm/      LlmAdapter seam + StreamChunk/BlockAssembler + adapters
```

The kernel crates are **GUI-free**: no `gpui`, no `wgpu`, no `rcad-*`, no `ocad-*`.
That is enforced by CI (see the architecture doc §3.2), and it is what lets the
kernel serve a CLI, a test, or an MCP server without dragging a UI along.

## Develop

```bash
cargo check --workspace
cargo test  --workspace
```

## License

See the per-crate `license` field. The extracted crates currently carry the
upstream (`Proprietary`) value; the framework's intended license is an open
question tracked in the architecture doc.
