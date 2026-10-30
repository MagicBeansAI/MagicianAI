const test = require('node:test');
const assert = require('node:assert/strict');

test('page-level Ask Magican opens on web pages and local files only', async () => {
    const created = [];
    let onClicked = null;
    globalThis.chrome = {
        runtime: { lastError: undefined },
        contextMenus: {
            remove(_id, callback) {
                callback();
            },
            create(properties, callback) {
                created.push(properties);
                callback?.();
            },
            onClicked: {
                addListener(listener) {
                    onClicked = listener;
                }
            }
        },
        windows: {
            async get() {
                return { left: 0, top: 0, width: 1000, height: 800 };
            }
        }
    };
    const posted = [];
    globalThis.fetch = async (url, init) => {
        posted.push({ url, body: JSON.parse(init.body) });
        return { ok: true, status: 200 };
    };

    const menu = await import('./contextual_assist_menu.js');
    menu.setupContextualAssistContextMenu();

    const pageItem = created.find((item) => item.id === 'magician_contextual_assist_page');
    assert.ok(pageItem, 'page-level menu item is registered');
    assert.deepEqual(pageItem.documentUrlPatterns, ['http://*/*', 'https://*/*', 'file:///*']);

    const click = async (pageUrl) => {
        posted.length = 0;
        onClicked(
            { menuItemId: 'magician_contextual_assist_page', pageUrl },
            { id: 3, windowId: 9, title: 'Matrix', url: pageUrl }
        );
        await new Promise((resolve) => setTimeout(resolve, 0));
        return posted.slice();
    };

    const fileUrl = 'file:///Volumes/build/magician/qualification/matrix.html';
    const fromFile = await click(fileUrl);
    assert.equal(fromFile.length, 1, 'a local file opens the native page menu');
    assert.match(fromFile[0].url, /\/host\/contextual-assist\/open$/);
    assert.equal(fromFile[0].body.state, 'page-context');
    assert.equal(fromFile[0].body.url, fileUrl);

    assert.equal((await click('https://example.com/')).length, 1);
    assert.equal((await click('chrome://settings/')).length, 0, 'browser pages stay excluded');
});
