# Self-hosted runners

## The problem this solves

GitHub-hosted runner *allocation* can be blocked account-wide — a spend limit reached, Actions
disabled at the repo/org level, or the account flagged for review. The recognizable signature is
every job, on every workflow, dying in roughly 1-3 seconds with `runner_id: 0` and an empty
`runner_name` — including a job as trivial as the licence gate, which does nothing but run one
shell script. That happens *before* any step in these workflow files executes, so no workflow-file
change can route around it while every job still targets a hosted `-latest` label — the block sits
between "a job exists" and "a runner ever picks it up."

A self-hosted runner sidesteps that entirely, because it never goes through hosted-runner
allocation. It is a small agent process you run yourself, on your own machine, that polls GitHub and
picks up jobs addressed to it. It either has network access and is running, or it doesn't — nothing
about hosted billing or quota touches it.

## How the toggle works

Every `runs-on` in `rust.yml`, `release.yml`, and `android.yml` reads a repo variable with a
hosted-runner fallback, e.g.:

```yaml
runs-on: ${{ vars.LUMEN_RUNNER_LINUX || 'ubuntu-latest' }}
```

Unset (the default), this is byte-for-byte the same as before — nothing changes until you set one.
The variables, set under **Settings → Secrets and variables → Actions → Variables** (repo variables,
not secrets — the value is just a runner label, nothing sensitive):

| Variable                       | Used by                                                          | Default          |
|---------------------------------|-------------------------------------------------------------------|-------------------|
| `LUMEN_RUNNER_LINUX`            | `rust.yml` (lint, licence gate, ubuntu test leg), `release.yml` (linux-x86_64, publish), `android.yml` (build, publish) | `ubuntu-latest`  |
| `LUMEN_RUNNER_WINDOWS`          | `rust.yml` (windows test leg), `release.yml` (windows-x86_64)     | `windows-latest` |
| `LUMEN_RUNNER_MACOS`            | `rust.yml` (macos test leg), `release.yml` (macos-aarch64, macos-x86_64) | `macos-latest`   |
| `LUMEN_RUNNER_ANDROID_EMULATOR` | `android.yml` (emulator)                                          | `ubuntu-latest`  |

Set a variable's value to whatever label you give the runner when you register it (GitHub defaults
new self-hosted runners to the label `self-hosted`; you can add a more specific custom label during
`config.sh`/`config.cmd`, e.g. `lumen-win-pc`, and use that instead — useful once you have more than
one self-hosted machine).

## Registering the Windows PC as a runner

This repo already has one real machine to point at: the Windows PC `device/windows-pc` is built and
tested on. To register it:

1. **Settings → Actions → Runners → New self-hosted runner**, choose Windows/x64. GitHub shows a
   PowerShell snippet with a repo-scoped registration token baked in — copy it as given, the token
   is single-use and short-lived.
2. Run the snippet: it downloads the runner, then `config.cmd` asks for a name and a label (default
   `self-hosted` is fine, or set a custom one — see above).
3. Install it as a Windows service rather than leaving a console window open, the same reasoning as
   `scripts/windows/Install-LumenServeTask.ps1` for `lumen serve` itself: `.\svc.sh install` /
   `.\svc.sh start` (via the runner's own `config.cmd` service prompt, or `run.cmd` for a foreground
   test first). A runner that dies when you sign out defeats the point.
4. Set `LUMEN_RUNNER_WINDOWS` to the label from step 2.

Only the Windows leg of `test`/`build` moves — `LUMEN_RUNNER_LINUX`/`LUMEN_RUNNER_MACOS` stay unset
and keep using hosted runners, so this alone does **not** get you out of an account-wide block; it
only helps once at least the Windows jobs are unblocked while Linux/macOS jobs still are, or once
you register additional machines for the rest.

## What's realistic to self-host here, and what isn't

- **Windows jobs** — realistic. You have the machine.
- **`LUMEN_RUNNER_LINUX` jobs** (lint, licence gate, the ubuntu test leg, Android build/publish) —
  realistic even without a spare Linux box: install the runner agent inside WSL2 on the same Windows
  PC. It's a normal Linux userspace process there; no virtualization concerns for these particular
  jobs since none of them need KVM.
- **macOS jobs** — only realistic if an actual Mac gets registered. There is no way to satisfy
  `LUMEN_RUNNER_MACOS` from a Windows or Linux box; leave it unset (hosted `macos-latest`) until one
  exists.
- **`LUMEN_RUNNER_ANDROID_EMULATOR`** — the hardest one. It needs KVM, i.e. hardware virtualization
  actually exposed to the runner process, not just a Linux userspace. WSL2 does not expose that by
  default; getting it working means either a real Linux machine, or nested virtualization
  specifically enabled for WSL2 (`.wslconfig`'s `nestedVirtualization=true` plus a Hyper-V host that
  itself allows it) — worth attempting only if the plain Linux/Windows jobs above already prove the
  self-hosted path works. Until then, leave this one unset.

## Security note

Self-hosted runners execute whatever a triggering workflow run tells them to, with whatever access
the machine they're installed on has. That's a real risk on repos that run untrusted code from fork
PRs — but none of `rust.yml`, `release.yml`, or `android.yml` has a `pull_request` trigger (see each
file's own `on:` block comment for why: every PR here is a same-repo branch, so `push` already
covers it). Only pushes made directly to a branch in this repo can reach a self-hosted runner, which
is the same trust boundary that already applies to pushing to this repo in the first place.
