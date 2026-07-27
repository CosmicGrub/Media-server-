# 07 — The Optional AI Agent

## 1. Design stance

The agent must be **genuinely optional and genuinely useful**. Those two constraints kill most designs:

- **Optional** means: a separate process, not linked into the server; zero cost when disabled (no model loaded, no
  memory, no background jobs, no network); the server's feature set is complete without it; and deleting the agent
  binary breaks nothing.
- **Useful** means: it does things that are *hard without an LLM*. A chat sidebar that answers "how do I add a
  library?" is not useful — that's documentation. The features in §5 are things you cannot build any other way.

**Default: OFF.** First-run does not mention it. It lives in Settings → Labs, with an honest description of what
it does, where data goes, and what it costs.

## 2. Architecture

```
┌──────────────────────┐        MCP over local socket / HTTP+SSE        ┌───────────────────┐
│    lumen-agent       │◄──────────────────────────────────────────────►│   lumen-server    │
│  (separate process   │                                                │  lumen-agent-mcp  │
│   or container)      │                                                │  (tool surface)   │
│                      │                                                └────────┬──────────┘
│  ┌────────────────┐  │                                                         │
│  │ Model backend  │  │   local:  llama.cpp / Ollama / ONNX Runtime              ▼
│  │  (pluggable)   │  │   cloud:  Claude API / OpenAI-compatible endpoint   ┌─────────┐
│  └────────────────┘  │                                                    │  DB /   │
│  ┌────────────────┐  │                                                    │ library │
│  │ Policy engine  │  │   allowlist, dry-run, confirmation, budget caps     └─────────┘
│  └────────────────┘  │
│  ┌────────────────┐  │
│  │  Audit log     │  │   append-only, user-readable, exportable
│  └────────────────┘  │
└──────────────────────┘
```

Key properties:
- The agent talks to the server through **the same MCP tool surface** any other client could use — no privileged
  back door, no direct DB access. If the agent can do it, an admin could do it through the API, and it shows up in
  the same audit log.
- **The agent is never in the media path.** It cannot touch a playback session, a decode pipeline, or a transcode
  stream. It reads state and proposes changes.
- **Model backend is pluggable and local-first.** Ship with a small local model as the default; cloud is opt-in with
  an explicit data-egress consent screen naming exactly what leaves the machine.

### 2.1 Model recommendations

| Tier | Backend | Hardware | Good for |
|---|---|---|---|
| **Embeddings only** (default when agent is on) | ONNX Runtime + `bge-small-en-v1.5` or `all-MiniLM-L6-v2` (~90 MB) | Any CPU | Semantic search (§5.1). No generation, no chat, negligible cost. **This alone justifies the feature.** |
| **Local small** | llama.cpp / Ollama, an 8B-class instruct model, Q4_K_M (~5 GB) | 8 GB RAM, or any GPU | Metadata cleanup, match disambiguation, natural-language search, basic triage |
| **Local large** | 30–70B class, Q4 (~20–40 GB) | 32 GB+ RAM or 24 GB VRAM | Everything, well |
| **Cloud** | Claude API (`claude-opus-5` / `claude-sonnet-5`) or any OpenAI-compatible endpoint | none | Best quality; opt-in only; per-request token budget cap enforced by the policy engine |

Detect available hardware at enable-time and recommend a tier rather than making the user guess.

## 3. The MCP tool surface

Tools are grouped by risk class. **Read tools are always available when the agent is on; write tools are opt-in
individually; destructive tools do not exist.**

### 3.1 Read (safe, always on)
| Tool | Purpose |
|---|---|
| `library.search` | Structured + FTS + vector search over items |
| `library.get_item` | Full metadata for an item incl. streams and files |
| `library.stats` | Counts, sizes, codec/resolution/HDR distribution, growth over time |
| `library.find_duplicates` | Via `content_sketch` and title/year clustering |
| `library.find_problems` | Missing metadata, unmatched, `needs_review`, missing episodes, corrupt probes, missing subtitles |
| `subtitles.search` | Dialogue search with timestamps |
| `playback.history` | Watch history, per-user, aggregated |
| `playback.report` | The `PlaybackReport` for a session — why it transcoded |
| `server.health` | CPU/RAM/disk/IO, job queue depth, transcode sessions, error rates |
| `server.logs` | Recent structured logs, filtered, **redacted** |
| `server.config_get` | Non-secret config |
| `jobs.list` | Scan/metadata/transcode job status |

### 3.2 Write (individually opt-in, always diff-then-confirm)
| Tool | Guardrail |
|---|---|
| `library.propose_match` | Produces a **proposal**, never applies. User approves in a review queue. |
| `library.edit_metadata` | Field-level, journalled to `item_revision`, one-click undo. Respects field locks. |
| `library.set_collection` / `tag` | Same |
| `subtitles.fetch` / `subtitles.resync` | Writes sidecars only, never modifies media files |
| `jobs.enqueue` | Only whitelisted job types; rate-limited; cannot enqueue unbounded work |
| `server.config_set` | Allowlisted keys only; never auth, never paths, never network exposure |
| `notify.send` | Through the notification plugin layer |

### 3.3 Explicitly absent — by design
`file.delete`, `file.move`, `file.write`, `shell.exec`, `user.create`, `user.grant`, `auth.*`, `network.expose`,
`plugin.install`. **The agent cannot delete or move a single byte of media, ever.** If a user wants agent-driven
file operations, they can export the agent's *recommendation* as a script and run it themselves. This is a hard
line; crossing it turns a helpful feature into an unbounded liability.

## 4. Guardrails (non-negotiable)

1. **Read-only by default.** Every write tool is a separate toggle with a plain-English description.
2. **Diff-then-confirm.** Every mutation is a proposal rendered as a before/after diff, batched into a review queue.
   An "auto-apply high-confidence proposals" toggle exists but is off, and is scoped per tool.
3. **Full audit log.** Append-only, human-readable, exportable: prompt, tools called, arguments, results, what was
   applied. The user can see exactly what the agent did and why.
4. **Undo.** Backed by `item_revision`. Any agent-applied change reverts in one click, including in bulk.
5. **Budgets.** Hard caps on tokens/requests/cost per hour and per day, enforced by the policy engine, not by the
   model. Exceeding a budget stops the agent, it doesn't degrade silently.
6. **Data egress consent.** For cloud backends, a one-time screen that names exactly what is sent (item titles,
   filenames, metadata, log excerpts) and offers a redaction profile. Filenames can leak a lot; default to
   aggressive redaction and let the user relax it.
7. **Never in the media path** (repeated because it matters).
8. **Prompt-injection containment.** Media metadata, NFO files, subtitle text, and provider responses are all
   **untrusted input**. Wrap every piece of external content in a clearly delimited envelope, instruct the model to
   treat it as data, and — critically — **rely on the tool allowlist, not the prompt, for safety**. A jailbroken
   agent still cannot delete a file, because no such tool exists. This is why §3.3 is the real defence.
9. **Local-first.** Default backend is local. If no local model is available, the agent runs in
   embeddings-only mode rather than silently reaching for a cloud API.
10. **Kill switch.** One toggle disables everything and unloads the model immediately.

## 5. Features that justify its existence

### 5.1 Semantic & dialogue search *(the killer feature — ship this first)*
Built on the subtitle FTS index and the local embedding index ([`05-server-library.md`](05-server-library.md) §10):

- *"The episode where they get stuck in the elevator"* → ranked scene matches with timestamps and thumbnails, one
  click to play from 34:12.
- *"Movies like Blade Runner but funnier"* → semantic similarity over overviews, genres, and watch history.
- *"That scene where the guy says something about a boat"* → dialogue vector search.
- *"Show me everything with Roger Deakins as cinematographer"* → structured query the UI never exposed.

Note this tier needs **no generative model at all** — an embedding model is enough. It works on a Raspberry Pi and
sends nothing anywhere. Make it the default behaviour when the agent is enabled.

### 5.2 Match disambiguation
The `needs_review` queue (from the Match stage) is the agent's best job. Given a filename, folder context, runtime,
stream layout, and the top candidates, an LLM resolves ambiguity far better than a scoring heuristic — especially
for foreign titles, remakes, anime numbering, and sports/concert content. Output is always a **proposal**.

### 5.3 Library curation
- *"Build a Halloween collection"* / *"Make a playlist for a 6-year-old's birthday party"* → collection proposals.
- Auto-generated collections from watch patterns.
- Duplicate resolution advice: *"You have Dune (2021) three times: 4K remux 78 GB, 1080p 12 GB, 720p 2 GB. The 720p
  copy has never been played. Reclaim 2 GB?"* — a recommendation, with a copy-able script. Never an auto-delete.

### 5.4 Operator triage — "why is this bad?"
Fed `PlaybackReport` + `server.health` + logs:
> *"Your 4K remux transcoded because the Chromecast reports no HEVC support. The transcode used software encoding
> because no GPU is available, which is why it stuttered. Options: (a) enable Direct Play by casting from a device
> that supports HEVC, (b) add an Intel Arc A310 (~$100) for hardware transcoding, (c) keep a 1080p H.264 version of
> your top 20 films — I can queue that job."*

This is the highest-value use of an LLM in this product. Every media-server community is full of people who cannot
diagnose this, and the structured `Reason` data from the playback ladder makes it tractable.

### 5.5 Quality & integrity audit
- Find files whose probe failed, whose duration doesn't match metadata, whose bitrate is anomalously low for their
  claimed resolution ("this '4K remux' is a 2 Mbps upscale"), whose audio is silent, or whose subtitles are
  out of sync (detectable by comparing subtitle timing to speech-activity detection).
- Upgrade candidates: *"These 40 films are 720p; you have the disk space and your other copies are 4K."*
- Storage forecasting from growth trends.

### 5.6 Natural-language configuration & operations
*"Only scan the media drives between 2 and 6 am"*, *"Notify me on Discord when a scan fails"*, *"Set my kids' profile
to PG-13 and hide the horror library"* → config proposals with diffs, applied only on confirmation, only for
allowlisted keys.

### 5.7 Subtitle work
Auto-resync (audio-alignment-driven, deterministic — the LLM just picks the strategy), translation of subtitles into
a missing language (with a clear "machine translated" label), and generation of subtitles for files with none via a
local Whisper-class model as a background job.

### 5.8 Voice control (Phase 4)
Local wake-word + local STT (Whisper) → the same tool surface. *"Play the next episode of The Bear on the living
room TV."* No cloud, and it reuses everything above.

## 6. What it must NOT be

- Not a chatbot pinned to the sidebar answering FAQs.
- Not required for any core function.
- Not a background daemon that "watches your library" and generates unsolicited notifications by default.
- Not a reason to send anyone's library contents to a third party without explicit, informed, revocable consent.
- Not something that can delete, move, or overwrite media.

## 7. Implementation notes

- Implement `lumen-agent-mcp` as a standard **MCP server** so the same tool surface works with Claude Desktop,
  Claude Code, or any MCP client — users who already have an AI setup get integration for free, and you get an
  ecosystem without building a chat UI.
- Structure the agent loop as: tool-call planning → execution → proposal generation → human review. Never a raw
  free-running loop with write access.
- Keep the system prompt small and the **tool descriptions rich** — that's where model behaviour actually comes from.
- Log token usage per feature so you can tell users what the agent costs them.
- Evaluate with a fixed test set: 200 ambiguous filenames with known-correct matches, 50 diagnostic scenarios with
  known root causes. Track accuracy per model tier so you can honestly say "the 8B local model resolves 78% of
  ambiguous matches; Claude resolves 94%."
