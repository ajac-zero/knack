# Static knack-registry deployment on Cloudflare

This example hosts a knack registry as static files in Cloudflare R2,
served by a small Cloudflare Worker. The Worker implements the
endpoints the knack CLI talks to — `/info`, `/index`, `/search`,
`/skills/<namespace>/<name>/archive` (namespaced canonical form), and
`/skills/<name>/archive` (legacy soft-resolved form for pre-namespacing
clients) — on top of pre-built artifacts produced by
`knack-registry build-static`.

At the scale of a curated public registry (~200 skills, single-digit
thousands of requests/day) this fits comfortably in Cloudflare's
free tier:

| Resource     | Free tier             | Expected usage at our scale |
|--------------|-----------------------|-----------------------------|
| R2 storage   | 10 GB/month           | ~6 MB                       |
| R2 reads     | 1M class-A ops/month  | well under                  |
| Worker invocations | 100k requests/day | well under                  |

## Why static?

Compared to running the live `knack-registry serve`:

- Zero runtime compute. No container, no persistent volume, no
  monitoring stack. R2 + Worker + a cron job is the whole shape.
- Edge-cached globally. Cloudflare's CDN puts the JSON and tarballs
  at every PoP; users worldwide get sub-100ms responses.
- Refresh latency = your cron interval, not the live registry's
  `--refresh-interval-seconds`. For a curated public registry where
  skills don't change minute-by-minute, daily cron is plenty (this
  example uses 06:00 UTC). Bump cadence per your taste; this is the
  one knob that costs nothing to change.

The live shape stays the right choice for internal registries
(private sources, auth, sub-minute publish-and-test loop). They're
complementary, not competing.

## Deployment flow

```
   ┌──────────────────┐       daily cron        ┌────────────────┐
   │  knack repo +    │ ──────────────────────▶ │  build job     │
   │  registries/*.toml│   (GitHub Actions /     │ knack-registry │
   └──────────────────┘    self-hosted runner)   │ build-static   │
                                                  └────────┬───────┘
                                                           │ upload
                                                           ▼
                                                  ┌────────────────┐
                                                  │  Cloudflare R2 │
                                                  │  bucket        │
                                                  └────────┬───────┘
                                                           │ R2.get()
                                                           ▼
   ┌──────────────┐    /info /index /search    ┌────────────────┐
   │  knack CLI   │ ◀───── /skills/X/archive ──│ Cloudflare     │
   │  (users)     │                            │ Worker         │
   └──────────────┘                            └────────────────┘
```

## One-time setup

```bash
# 1. Install wrangler if you haven't.
npm install -g wrangler

# 2. Authenticate.
wrangler login

# 3. Create the R2 bucket. Name it whatever you want; update wrangler.toml
#    to match.
wrangler r2 bucket create knack-registry-public

# 4. (Optional) Set a custom domain. Cloudflare dashboard -> Workers & Pages
#    -> your Worker -> Settings -> Triggers -> Add Custom Domain.
#    Or via wrangler.toml's [[routes]] block.

# 5. Deploy the Worker.
wrangler deploy
```

## Build and upload (run from the repo root)

```bash
# 1. Materialise the static snapshot.
cargo run --release -p knack-registry -- \
    build-static \
        --index registries/public.toml \
        --output ./dist \
        --name public

# 2. Upload to R2. The bucket's existing contents are overwritten.
#    (The Worker uses fresh lookups, no per-key purge needed.)
cd dist
for file in info.json index.json sha-map.json; do
    wrangler r2 object put knack-registry-public/$file --file=$file
done
# `find` rather than `skills/*` so we recurse into namespace directories
# (skills/<namespace>/<name>.skill.tar.gz) added when build-static
# materialised scoped skills.
find skills -type f -name '*.skill.tar.gz' | while read tarball; do
    wrangler r2 object put "knack-registry-public/$tarball" --file="$tarball"
done
```

A more efficient bulk uploader (parallel, hash-skip) is the
`@cloudflare/r2-upload` action or `rclone` against the S3-compatible
R2 endpoint — left as an exercise depending on your CI choice.

## Suggested GitHub Actions workflow

```yaml
# .github/workflows/publish-public-registry.yml
name: Publish public registry
on:
  schedule:
    - cron: '0 6 * * *'           # daily at 06:00 UTC
  workflow_dispatch:               # also allow manual runs

jobs:
  publish:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: actions/cache@v4
        with:
          path: |
            ~/.cargo/registry
            ~/.cargo/git
            target
          key: cargo-${{ hashFiles('**/Cargo.lock') }}
      - uses: dtolnay/rust-toolchain@stable
      - run: cargo build --release -p knack-registry
      - run: ./target/release/knack-registry build-static
              --index registries/public.toml
              --output ./dist
              --name public
      # Use wrangler in CI (assumes you've set CLOUDFLARE_API_TOKEN
      # and CLOUDFLARE_ACCOUNT_ID as secrets).
      - run: npm install -g wrangler
      - name: Upload static artifacts to R2
        env:
          CLOUDFLARE_API_TOKEN: ${{ secrets.CLOUDFLARE_API_TOKEN }}
          CLOUDFLARE_ACCOUNT_ID: ${{ secrets.CLOUDFLARE_ACCOUNT_ID }}
        run: |
          cd dist
          for file in info.json index.json sha-map.json; do
              wrangler r2 object put knack-registry-public/$file --file=$file
          done
          # `find` rather than `skills/*` so nested namespace dirs
          # (skills/<namespace>/<name>.skill.tar.gz) are picked up.
          find skills -type f -name '*.skill.tar.gz' | while read tarball; do
              wrangler r2 object put "knack-registry-public/$tarball" --file="$tarball"
          done
```

## Verifying the deployed registry

This repo's deployment lives at https://knack.ajac-zero.com — replace
that with your own domain when verifying a fork. Registry name is
`public` (set via `--name` on the build-static job).

```bash
# Pretend to be the knack CLI.
curl https://knack.ajac-zero.com/info
curl https://knack.ajac-zero.com/index | jq '.skill | length'
curl 'https://knack.ajac-zero.com/search?q=pdf' | jq

# Namespaced (canonical) — direct R2 lookup.
curl -D - https://knack.ajac-zero.com/skills/anthropics/pdf/archive \
    -o /tmp/pdf.tgz | grep -i x-knack
# Expect both X-Knack-Resolved-Sha AND X-Knack-Namespace: anthropics.

# Legacy bare — soft-resolves via index.json. Same response when the
# bare name is unique across all namespaces; 409 with disambiguation
# when ambiguous.
curl -D - https://knack.ajac-zero.com/skills/pdf/archive \
    -o /tmp/pdf.tgz | grep -i x-knack

tar -tzf /tmp/pdf.tgz | head

# Or use the actual CLI.
knack registry add https://knack.ajac-zero.com
knack find pdf
knack add public:anthropics/pdf      # canonical
knack add public:pdf                 # legacy bare, registry resolves
```

## What this Worker does NOT do

- **Authentication**. The static snapshot is public; any client that
  knows the URL can fetch any indexed skill. For a public registry
  that's the point.
- **Webhooks / push triggers**. The build is cron-driven. To force
  an immediate rebuild, trigger the GitHub Action manually.
- **Per-tenant / per-namespace serving**. One bucket, one registry.
  Run multiple Workers + buckets if you need multiple registries.
- **Server-side index mutation**. Everything is bake-and-publish. To
  change the registry's contents, edit `registries/public.toml` in
  the repo and wait for the next cron tick (or trigger manually).
