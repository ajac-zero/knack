// knack-registry static deployment — Cloudflare Worker
//
// Serves a snapshot produced by `knack-registry build-static --output ./dist`
// out of an R2 bucket. Implements the four endpoints the knack CLI talks to.
//
// Setup:
//   1. Create an R2 bucket (e.g. `knack-registry-public`).
//   2. Upload the contents of ./dist into it. Layout:
//        info.json
//        index.json
//        sha-map.json
//        skills/<namespace>/<name>.skill.tar.gz   ← scoped (namespacing-aware)
//        skills/<name>.skill.tar.gz               ← unscoped (legacy)
//   3. Bind the bucket as `BUCKET` in wrangler.toml.
//   4. Deploy this worker.
//
// Endpoints implemented:
//   GET /info                              -> info.json
//   GET /index                             -> index.json
//   GET /search?q=<terms>                  -> filters index.json server-side
//   GET /skills/<ns>/<name>/archive        -> namespaced; direct R2 lookup
//   GET /skills/<name>/archive             -> legacy; soft-resolves via index
//
// All responses are cached at the edge. Set `CACHE_TTL_SECONDS` to control
// how aggressively. The default is 60s, which balances staleness against
// origin (R2) load. Static files at this scale fit easily in free tier.

const CACHE_TTL_SECONDS = 60;

export default {
    async fetch(request, env) {
        const url = new URL(request.url);
        const path = url.pathname;

        // Health endpoint mirrors the live registry's /health for clients
        // and uptime checks that probe both shapes interchangeably.
        if (path === '/health') {
            return new Response('ok', {
                headers: { 'content-type': 'text/plain' },
            });
        }

        if (path === '/info') {
            return serveJsonFromR2(env, 'info.json');
        }

        if (path === '/index') {
            return serveJsonFromR2(env, 'index.json');
        }

        if (path === '/search') {
            const query = url.searchParams.get('q') || '';
            return handleSearch(env, query);
        }

        // Namespaced route takes precedence over legacy — match it first so
        // a request like /skills/vercel/find-skills/archive doesn't try to
        // resolve "vercel" as a bare skill name.
        const nsArchive = path.match(/^\/skills\/([^/]+)\/([^/]+)\/archive$/);
        if (nsArchive) {
            return handleNamespacedArchive(env, nsArchive[1], nsArchive[2]);
        }

        // Legacy single-segment archive — soft-resolves a bare name against
        // index.json so pre-namespacing knack CLIs and pre-migration
        // manifests/lockfiles keep working after the registry upgrade.
        const legacyArchive = path.match(/^\/skills\/([^/]+)\/archive$/);
        if (legacyArchive) {
            return handleLegacyArchive(env, legacyArchive[1]);
        }

        return new Response('not found', { status: 404 });
    },
};

// ---- handlers -----------------------------------------------------------

async function serveJsonFromR2(env, key) {
    const obj = await env.BUCKET.get(key);
    if (!obj) {
        return new Response(`missing ${key}`, { status: 500 });
    }
    return new Response(obj.body, {
        headers: {
            'content-type': 'application/json',
            'cache-control': `public, max-age=${CACHE_TTL_SECONDS}`,
        },
    });
}

// Mirrors knack-core's IndexedSkill::search: lowercase all terms, all terms
// must appear in (namespace + name + description + tags) lowercase. The
// namespace inclusion mirrors the live registry's search_text() so
// `knack find anthropics` lists everything from that vendor without users
// having to remember individual skill names.
//
// The whole index is loaded per request — at our scale (a few hundred KB)
// this is cheap, and Cloudflare's HTTP cache layer (set above via
// cache-control) means most requests don't touch the Worker at all.
async function handleSearch(env, query) {
    const terms = query.toLowerCase().split(/\s+/).filter(Boolean);
    if (terms.length === 0) {
        return jsonResponse([]);
    }

    const indexObj = await env.BUCKET.get('index.json');
    if (!indexObj) {
        return new Response('index missing', { status: 500 });
    }
    const index = await indexObj.json();

    const matches = (index.skill || []).filter((skill) => {
        const haystack = [
            skill.namespace || '',
            skill.name,
            skill.description,
            (skill.tags || []).join(' '),
        ]
            .join(' ')
            .toLowerCase();
        return terms.every((term) => haystack.includes(term));
    });

    return jsonResponse(matches);
}

// Namespaced archive: direct R2 lookup using the qualified key. Sets
// X-Knack-Namespace (echoing the URL) and X-Knack-Resolved-Sha (from
// sha-map.json) so the CLI can persist both into the lockfile.
async function handleNamespacedArchive(env, namespace, name) {
    const qualified = `${namespace}/${name}`;
    const tarballKey = `skills/${qualified}.skill.tar.gz`;
    return serveArchive(env, tarballKey, qualified, namespace, name);
}

// Legacy single-segment archive: scan index.json for any entry whose BARE
// name matches.
//
//   - exactly one match  → 200 + X-Knack-Namespace set to the resolved
//                          scope. Mirrors the live registry's soft-resolve
//                          so the lockfile gets the canonical namespace
//                          even when the user typed the bare form.
//   - several matches    → 409 Conflict with a disambiguation hint listing
//                          the available namespaced forms.
//   - zero matches       → 404 (after trying the flat-layout R2 key as a
//                          last resort, for snapshots with unscoped skills
//                          using the legacy build-static layout).
async function handleLegacyArchive(env, name) {
    const indexObj = await env.BUCKET.get('index.json');
    if (!indexObj) {
        return new Response('index missing', { status: 500 });
    }
    const index = await indexObj.json();

    const matches = (index.skill || []).filter((skill) => skill.name === name);

    if (matches.length === 1) {
        const skill = matches[0];
        const qualified = skill.namespace ? `${skill.namespace}/${name}` : name;
        const tarballKey = `skills/${qualified}.skill.tar.gz`;
        return serveArchive(env, tarballKey, qualified, skill.namespace || null, name);
    }

    if (matches.length > 1) {
        const qualifieds = matches
            .map((s) => (s.namespace ? `${s.namespace}/${name}` : name))
            .join(', ');
        return new Response(
            `skill \`${name}\` is ambiguous across namespaces: [${qualifieds}]; ` +
                'retry as one of the namespaced forms above',
            { status: 409 },
        );
    }

    // Zero index matches — last-resort try the flat-layout R2 key for
    // snapshots that include unscoped skills authored before namespacing.
    // No X-Knack-Namespace header in that case.
    const tarballKey = `skills/${name}.skill.tar.gz`;
    const obj = await env.BUCKET.get(tarballKey);
    if (!obj) {
        return new Response('not found', { status: 404 });
    }
    return serveArchive(env, tarballKey, name, null, name);
}

// Shared response builder. Looks up the tarball in R2, sets the response
// headers (Content-Type, Content-Disposition, X-Knack-Resolved-Sha when
// available, X-Knack-Namespace when scoped), and streams the body.
async function serveArchive(env, tarballKey, qualified, namespace, bareName) {
    const obj = await env.BUCKET.get(tarballKey);
    if (!obj) {
        return new Response('not found', { status: 404 });
    }

    // X-Knack-Resolved-Sha — the CLI pins this into the lockfile's
    // `resolved` field so peers reinstall from the same content. Header is
    // omitted when sha-map.json doesn't list this skill (e.g. snapshots
    // built from a source without resolvable git history).
    const shaMapObj = await env.BUCKET.get('sha-map.json');
    let sha = null;
    if (shaMapObj) {
        const shaMap = await shaMapObj.json();
        sha = shaMap[qualified] || null;
    }

    const headers = new Headers({
        'content-type': 'application/gzip',
        'content-disposition': `attachment; filename="${bareName}.skill.tar.gz"`,
        'cache-control': `public, max-age=${CACHE_TTL_SECONDS}`,
    });
    if (sha) {
        headers.set('x-knack-resolved-sha', sha);
    }
    if (namespace) {
        // X-Knack-Namespace lets a CLI that hit the legacy single-segment
        // URL learn which namespace served it, so it can persist that into
        // the lockfile and use the namespaced URL on subsequent syncs.
        headers.set('x-knack-namespace', namespace);
    }

    return new Response(obj.body, { headers });
}

function jsonResponse(data) {
    return new Response(JSON.stringify(data), {
        headers: {
            'content-type': 'application/json',
            'cache-control': `public, max-age=${CACHE_TTL_SECONDS}`,
        },
    });
}
