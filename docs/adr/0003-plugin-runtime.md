# ADR-0003 — Plugins run as sandboxed WebAssembly components

**Status:** Proposed (pending spike S7)
**Date:** 2026-07-27

## Context

The product must support downloadable plugins (metadata providers, subtitle providers, sync targets, PVR backends,
themes, shader packs) across servers on x86 and ARM NAS hardware, desktops, phones, and — ideally — the browser.

Prior art in this space is uniformly weak on security:
- **Kodi** runs Python 3 addons with **no sandbox** — an addon can execute arbitrary code with the user's privileges.
- **Jellyfin** loads plugins as trusted .NET assemblies in-process.
- **Plex** killed its plugin system in 2018 rather than solve the problem.

A media server has read access to a NAS and often write access to download directories, and it usually sits on a home
LAN with no internal segmentation. An unsandboxed plugin system is an RCE appliance.

## Decision

**Plugins are WebAssembly components, hosted by Wasmtime, with capability-based permissions declared in a manifest
and enforced by the host. Interfaces are defined in WIT (WebAssembly Component Model).**

- Plugins receive **no ambient authority**: no filesystem, no sockets, no clock, no environment. They get only the
  host functions their manifest requests and the user grants.
- Network access is exclusively through a host `fetch` function with a **manifest-declared host allowlist**,
  host-enforced rate limits, timeouts, and TLS.
- Memory, CPU fuel, and per-call wall clock are capped by the host.
- Two plugin classes are **declarative only** (no code at all): **themes/skins** (CSS/tokens/layout JSON) and
  **shader/enhancement packs** (`.glsl` + manifest). These cover the most-wanted community extensions with zero
  execution risk.
- A **trusted-native tier** exists only for things Wasm genuinely cannot do (hardware tuner drivers, exotic
  filesystems). It is signed by the project, requires an explicit user acknowledgement, and is deliberately tiny.
- All plugins are **signed** (minisign or Sigstore/cosign); unsigned plugins load only in developer mode.
- Adoption path: **start on Extism** (much lower ceremony, host and plugin SDKs in a dozen languages) to get an
  ecosystem moving quickly, and migrate to raw Wasmtime + WIT components once interfaces stabilize. Design the WIT
  interfaces first regardless.

## Rationale

1. **Capability-based sandboxing is the core property.** WASI components start with no authority and can only do what
   the host explicitly grants — a security model that maps directly onto a permission sheet a user can read and
   understand.
2. **One artifact, every platform.** The same `.wasm` runs on an x86 server, an ARM Synology, an Android phone, and
   in the browser. No other plugin technology gives you this, and it's essential because metadata providers need to
   run client-side in the server-optional mode.
3. **Any source language.** Rust, TinyGo, C/C++, Zig, JS/TS (ComponentizeJS), Python (componentize-py). Plugin
   authorship isn't gated on your language choice.
4. **Typed, versioned interfaces.** WIT gives compile-checked contracts instead of duck-typed dicts, so a host upgrade
   doesn't silently break every plugin.
5. **Maturity.** Wasmtime was the first runtime to fully implement WASI Preview 2 and the Component Model. The
   ecosystem is moving to Preview 3, which adds native `future`/`stream` types to WIT for non-blocking I/O across the
   component boundary — precisely what a network-fetching plugin wants.
6. **Resource limits are host-enforced**, so a buggy or hostile plugin cannot wedge the server.

## Alternatives rejected

| Alternative | Why rejected |
|---|---|
| Native `.so`/`.dll` plugins | ABI fragility, no sandbox, must be built per platform+arch, no browser story |
| Embedded Python (Kodi's model) | No sandbox, ~40 MB runtime, painful on mobile, per-platform packaging misery |
| Embedded JS (QuickJS/Deno) | Weaker isolation guarantees than Wasm, no capability model, harder to bound resources |
| Plugins as microservices over HTTP | Excellent isolation, unacceptable UX — nobody will run six containers to get subtitles |
| Lua | Small and embeddable, but no capability model and a narrow author pool |

## Consequences

**Positive**
- Installing a plugin is a bounded, reviewable action with a truthful permission prompt.
- Plugins work identically on every platform including the browser and the server-optional client mode.
- The host can revoke, kill, meter, and audit any plugin.
- Plugin authors aren't constrained to our language.

**Negative**
- Extra engineering: a host, WIT interfaces, bindings generation, developer tooling, and a registry with signing.
- Per-call overhead versus in-process calls (target: < 50 ms including instantiation — spike S7 measures this; expect
  to need **instance pooling** rather than per-call instantiation).
- Async I/O across the component boundary is awkward until WASI Preview 3 stabilizes; until then, one call = one
  blocking host call with a hard timeout.
- Plugin authors face a less familiar toolchain than "write a Python file." Mitigate with scaffolding templates,
  hot-reload dev tooling, and a fixture-based test harness — DX determines whether an ecosystem exists at all.

## References
- [WASI.dev — capability-based sandbox](https://wasi.dev/)
- [WASI & Component Model status](https://eunomia.dev/blog/2025/02/16/wasi-and-the-webassembly-component-model-current-status/)
- [Component Model in 2026 — Preview 3, async streams](https://blog.iamcristhian.dev/2026/04/webassembly-component-model-wasm-beyond-browser-2026)
- [Architecting agnostic plugin systems with the Component Model](https://techbytes.app/posts/wasm-component-model-plugin-architecture/)
- [WebAssembly ecosystem 2026](https://reintech.io/blog/webassembly-ecosystem-2026-tools-frameworks-runtimes)
