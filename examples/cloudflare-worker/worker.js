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
//        skills/<name>.skill.tar.gz
//   3. Bind the bucket as `BUCKET` in wrangler.toml.
//   4. Deploy this worker.
//
// Endpoints implemented:
//   GET /info                  -> info.json
//   GET /index                 -> index.json
//   GET /search?q=<terms>      -> filters index.json server-side
//   GET /skills/<name>/archive -> streams from R2, sets X-Knack-Resolved-Sha
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

        const archiveMatch = path.match(/^\/skills\/([^\/]+)\/archive$/);
        if (archiveMatch) {
            return handleArchive(env, archiveMatch[1]);
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
// must appear in (name + description + tags) lowercase. The whole index is
// loaded per request — at our scale (a few hundred KB) this is cheap, and
// Cloudflare's HTTP cache layer (set above via cache-control) means most
// requests don't touch the Worker at all.
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

async function handleArchive(env, name) {
    const tarballKey = `skills/${name}.skill.tar.gz`;
    const obj = await env.BUCKET.get(tarballKey);
    if (!obj) {
        return new Response('not found', { status: 404 });
    }

    // Look up the SHA for X-Knack-Resolved-Sha. The CLI uses this to pin
    // its lockfile's `resolved` field. Header is omitted when sha-map.json
    // doesn't list this skill (e.g. local skills_root entries — not used
    // in the static deployment but the contract is the same).
    const shaMapObj = await env.BUCKET.get('sha-map.json');
    let sha = null;
    if (shaMapObj) {
        const shaMap = await shaMapObj.json();
        sha = shaMap[name] || null;
    }

    const headers = new Headers({
        'content-type': 'application/gzip',
        'content-disposition': `attachment; filename="${name}.skill.tar.gz"`,
        'cache-control': `public, max-age=${CACHE_TTL_SECONDS}`,
    });
    if (sha) {
        headers.set('x-knack-resolved-sha', sha);
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
