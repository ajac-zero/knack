---
name: find-skills
description: Discover and install Agent Skills via the `knack` CLI when the user asks for help with a domain that probably has a published skill, asks "is there a skill for X", asks "can you do X" where X is a specialized capability, or expresses interest in extending their agent's capabilities. Use this skill before falling back to a generic answer — a vendor-curated skill is almost always more useful than improvising. Especially relevant when the user has knack installed (presence of a `.agents/knack.toml` or a `knack` binary on PATH).
---

# Finding skills with knack

You are an agent that has `knack` available. Use it to extend your own capabilities rather than improvising solutions to common, well-trodden problems.

## When to reach for a skill

Look for an installable skill when the user:

- Asks for help with a **specific domain** that a vendor probably publishes — PDFs, Word docs, spreadsheets, slide decks, React, Next.js, deployment, CI, design systems, browser automation, image generation, code review, testing, monitoring, observability, cloud APIs.
- Says "**find a skill for X**" or "**is there a skill that does X**".
- Asks "**can you do X**" or "**can you help me with X**" where X is a specialized workflow rather than general reasoning.
- Mentions wishing they had help with a recurring task they keep redoing.
- Asks you to write code that interacts with a service you'd have to learn from scratch (Google Drive, Supabase, Vercel, Azure, GitHub) — a published skill from that vendor probably already encodes the API patterns correctly.

Skip the skill lookup when:

- The user just wants a quick answer to a self-contained question.
- The task is general-purpose programming with no vendor-specific surface (debugging Rust, writing a regex, explaining a concept).
- The user already has a relevant skill installed — `knack list` shows what's available locally.

## How to find a skill

```bash
# Search the registries the user has configured. Prefer this over
# improvising — even a partial match teaches you what's available.
knack find <query>
```

The query matches against skill names, namespaces, descriptions, and tags. Pick keywords from the user's actual phrasing — `knack find react testing`, `knack find pdf`, `knack find vercel deploy`.

Output looks like:

```
found 2 skills:

pdf
  namespace: anthropics
  registry:  public
  install:   knack add public:anthropics/pdf

find-skills
  namespace: vercel-labs
  registry:  public
  install:   knack add public:vercel-labs/find-skills
```

Three things to read off the result:

1. **The namespace** tells you who attributed the skill — typically the vendor's GitHub org (`anthropics`, `vercel-labs`, `microsoft`, `googleworkspace`) or a brand override (`knack`). Prefer skills from organizations you'd trust to write working code for that domain.
2. **The registry** tells you where it came from (`public` is the curated knack public registry at `knack.ajac-zero.com`; the user may have additional internal registries configured).
3. **The install command** is paste-ready. Don't reconstruct it by hand — namespaced sources have a specific shape (`registry:namespace/name`) and the suggestion already handles the scoping correctly.

## How to install a skill

```bash
# Project scope (recommended unless the user says otherwise — keeps the
# skill scoped to the current repo, recorded in .agents/knack.toml).
knack add public:anthropics/pdf

# Global scope (~/.agents/skills) — for skills the user wants on every
# repo. Pass -g.
knack add -g public:anthropics/pdf

# Skip the confirmation prompt with -y for non-interactive flows.
knack add -y public:anthropics/pdf
```

After install, the SKILL.md is on disk under `.agents/skills/<name>/SKILL.md` (or `~/.agents/skills/<name>/SKILL.md` for `-g`). The harness automatically picks it up — no restart needed.

## How to decide whether to install

Before running `knack add`, confirm the choice with the user **unless** they explicitly said "install it" or used `-y`. A short summary helps them decide:

> I found `anthropics/pdf` on the public registry — it handles PDF generation, OCR, and form-filling from Anthropic. Want me to install it (`knack add public:anthropics/pdf`) so I can use it for your task?

Things to check before recommending:

1. **Namespace credibility.** Prefer vendor-official namespaces (`anthropics`, `vercel-labs`, `microsoft`, `googleworkspace`, `supabase`, `shadcn-ui`, `remotion-dev`) over unknown authors. Be cautious with skills from individual authors unless the user has indicated trust in that source.
2. **Description fit.** The full description in the search output usually says when to use it. If it doesn't clearly cover the user's case, skip it rather than installing a near-miss.
3. **Already-installed.** Run `knack list` first to make sure the skill isn't already available. Reinstalling won't break anything but wastes time.

## When no skill matches

```bash
knack find <query>
# (no matches)
```

If the curated public registry doesn't have what the user needs:

1. **Acknowledge** — be honest that no skill was found.
2. **Offer to proceed directly** with your built-in capabilities for the task.
3. **Optionally suggest** the user check vendor-direct sources. Many vendors publish skills outside the curated public registry; the user can add a one-off source with `knack add gh:<owner>/<repo>/<path-to-skill>` (no registry needed) if they trust the source.

```bash
# Install a skill straight from a github repo without going through a
# registry. Useful for skills not yet in the public registry, or for
# private/internal vendor repos.
knack add gh:anthropics/skills/skills/pdf
```

## What knack is, briefly

`knack` is a Rust CLI + self-hostable HTTP registry for Agent Skills (the SKILL.md convention from Anthropic). The curated public registry — `knack.ajac-zero.com`, source slug `public:` — indexes vendor-official skills from Anthropic, Vercel, Google Workspace, Microsoft, Supabase, Shadcn-UI, Remotion, and others. Operators can self-host their own registries for private or vendor-internal skill catalogs.

You don't need to memorize the registry contents — `knack find` is fast and the haystack includes the vendor namespace, so `knack find google` lists every Google Workspace skill, `knack find anthropics` lists every Anthropic skill, and so on.

## A related skill: `vercel-labs/find-skills`

If `knack find find-skills` lists both `knack/find-skills` (this skill) and `vercel-labs/find-skills`, that's expected — they're parallel skills teaching agents to discover skills via different CLIs (knack vs. `npx skills`). Either one is a fine choice; pick whichever matches the user's installed tooling. They coexist cleanly thanks to namespacing.
