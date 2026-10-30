// This page contains no provider HTML or third-party script. The capability
// stays in memory; neither tokens nor typed input are written to storage.
const root = '/api/magician/v2/auth/browser-signin';
const screen = document.getElementById('screen');
const status = document.getElementById('status');
const origin = document.getElementById('origin');
const text = document.getElementById('text');
let credential = location.hash.slice(1);
history.replaceState(null, '', location.pathname);
let sequence = 0;
let revision = '';
let closed = false;
let working = false;
let inputPending = false;
class PageChanged extends Error {}

async function request(action, body = {}) {
    const controller = new AbortController();
    const timeout = setTimeout(() => controller.abort(), 45000);
    try {
        const response = await fetch(`${root}/${action}`, {
            method: 'POST', cache: 'no-store', credentials: 'same-origin', redirect: 'error',
            headers: { 'Content-Type': 'application/json', 'X-Magician-Signin': credential },
            body: JSON.stringify(body), signal: controller.signal
        });
        const data = await response.json();
        if (!response.ok) {
            if (data.error === 'signin_page_changed') throw new PageChanged('The page changed before the input arrived. Review the picture and try again.');
            throw new Error('The connection could not continue. Reopen the sign-in request to retry.');
        }
        return data;
    } finally { clearTimeout(timeout); }
}

function enabled(value) {
    document.querySelectorAll('button:not(#close), input').forEach(element => { element.disabled = !value; });
}

async function refresh() {
    if (closed || working || inputPending || document.hidden) return;
    working = true;
    try {
        const frame = await request('frame');
        if (closed || document.hidden) return;
        if (frame.status !== 'waiting') {
            status.textContent = frame.status === 'connected' ? 'Connected. You can return to Magican.' : 'This sign-in has ended. Reopen the request if you still need to connect.';
            finish();
            return;
        }
        if (frame.retry_after_ms) { enabled(Boolean(revision) && !inputPending); return; }
        if (typeof frame.image !== 'string') {
            revision = ''; screen.removeAttribute('src'); enabled(false); origin.textContent = 'Loading…'; return;
        }
        screen.src = `data:image/jpeg;base64,${frame.image}`;
        screen.width = frame.width;
        screen.height = frame.height;
        origin.textContent = frame.origin;
        revision = frame.frame_revision;
        enabled(!inputPending);
    } catch (error) {
        status.textContent = error instanceof Error ? error.message : 'Connection interrupted.';
        enabled(false);
        // Do not replay input or poll a failed sign-in forever.
        finish();
    } finally { working = false; }
}

async function input(value) {
    if (closed || inputPending || !revision) return;
    const expectedRevision = revision;
    inputPending = true;
    enabled(false);
    status.textContent = '';
    while (working && !closed) await new Promise(resolve => setTimeout(resolve, 30));
    if (closed) { inputPending = false; return; }
    try { await request('input', { sequence: ++sequence, frame_revision: expectedRevision, input: value }); }
    catch (error) {
        status.textContent = `${error instanceof Error ? error.message : 'Input was not confirmed.'} Input is never retried automatically.`;
        if (error instanceof PageChanged) { revision = ''; screen.removeAttribute('src'); }
        else finish();
    }
    finally { inputPending = false; }
    await refresh();
}

function revoke(capability) {
    if (!/^bsi_[a-f0-9]{64}$/.test(capability)) return;
    void fetch(`${root}/close`, { method: 'POST', credentials: 'same-origin', cache: 'no-store', keepalive: true, redirect: 'error',
        headers: { 'Content-Type': 'application/json', 'X-Magician-Signin': capability }, body: '{}' }).catch(() => {});
}

function finish() {
    if (closed) return;
    closed = true;
    enabled(false);
    text.value = '';
    screen.removeAttribute('src');
    revoke(credential);
    credential = '';
}

screen.addEventListener('click', event => {
    const bounds = screen.getBoundingClientRect();
    void input({ kind: 'tap', x: (event.clientX - bounds.left) / bounds.width, y: (event.clientY - bounds.top) / bounds.height });
});
document.getElementById('up').onclick = () => void input({ kind: 'scroll', delta: -400 });
document.getElementById('down').onclick = () => void input({ kind: 'scroll', delta: 400 });
for (const [id, key] of [['tab', 'Tab'], ['enter', 'Enter'], ['backspace', 'Backspace']]) {
    document.getElementById(id).onclick = () => void input({ kind: 'key', key });
}
document.getElementById('typing').onsubmit = event => {
    event.preventDefault();
    if (inputPending || closed || !text.value) return;
    const value = text.value;
    text.value = '';
    void input({ kind: 'text', value });
};
document.getElementById('close').onclick = () => { status.textContent = 'Private browser closed. The sign-in request remains available in Magican.'; finish(); };
window.addEventListener('pagehide', finish);
document.addEventListener('visibilitychange', () => { text.value = ''; if (document.hidden) screen.removeAttribute('src'); else void refresh(); });
enabled(false);
try {
    if (!(location.protocol === 'https:' || (location.protocol === 'http:' && ['localhost', '127.0.0.1', '[::1]'].includes(location.hostname)))) {
        throw new Error('Use HTTPS to your Magican computer for private sign-in, or choose Open on computer.');
    }
    if (!/^bsi_[a-f0-9]{64}$/.test(credential)) throw new Error('Open this page from the sign-in request in Magican.');
    const session = await request('claim');
    if (closed) {
        revoke(session.session_token);
    } else if (session.status === 'connected') {
        status.textContent = 'Connected. You can return to Magican.';
        finish();
    } else {
        if (!/^bsi_[a-f0-9]{64}$/.test(session.session_token)) throw new Error('Invalid sign-in response.');
        credential = session.session_token;
        status.textContent = `Workspace: ${session.workspace}. Tap the page to complete sign-in.`;
        await refresh();
        const timer = setInterval(() => { if (closed) clearInterval(timer); else void refresh(); }, 700);
    }
} catch (error) {
    status.textContent = error instanceof Error ? error.message : 'Sign-in could not start.';
    finish();
}
