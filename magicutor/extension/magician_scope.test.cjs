const test = require('node:test');
const assert = require('node:assert/strict');

test('extension calls borrow the signed-in Magician session and retry once on 401', async () => {
    const tokens = ['stale-session', 'fresh-session'];
    let reads = 0;
    globalThis.chrome = {
        storage: {
            local: {
                async get() {
                    return {};
                }
            }
        },
        tabs: {
            async query(query) {
                if (query && query.url === 'http://localhost:5173/*') return [{ id: 7 }];
                return [];
            }
        },
        scripting: {
            async executeScript() {
                reads += 1;
                return [{ result: tokens.shift() || 'fresh-session' }];
            }
        }
    };
    const seen = [];
    globalThis.fetch = async (_url, init) => {
        seen.push(init.headers.get('Authorization'));
        return { status: seen.length === 1 ? 401 : 200 };
    };

    const scope = await import('./magician_scope.js');
    scope.clearMagicianBearerCache();
    const response = await scope.magicianFetch(
        'http://127.0.0.1:3002/api/magician/v2/meetings/whatsapp-ring',
        { method: 'GET' }
    );
    assert.equal(response.status, 200);
    assert.deepEqual(seen, ['Bearer stale-session', 'Bearer fresh-session']);
    assert.equal(reads, 2);

    globalThis.chrome.storage.local.get = async () => ({ magicianScopedBearer: 'installed-token' });
    scope.clearMagicianBearerCache();
    const before = reads;
    globalThis.fetch = async (_url, init) => {
        seen.push(init.headers.get('Authorization'));
        return { status: 200 };
    };
    await scope.magicianFetch('http://127.0.0.1:3002/api/magician/v2/health');
    assert.equal(seen.at(-1), 'Bearer installed-token');
    assert.equal(reads, before);

    await assert.rejects(
        () => scope.magicianFetch('https://example.com/meetings/whatsapp-ring'),
        /untrusted URL/
    );
});
