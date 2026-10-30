// Exercise the pinned upstream command parser with a fake transport. No auth,
// sockets, real recipients or local WhatsApp state are loaded.
const test = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const vm = require('node:vm');
const { createRequire } = require('node:module');

const packageRoot = process.env.WU_TEST_PACKAGE_ROOT || path.resolve(__dirname, '../../node_modules/@ibrahimwithi/wu-cli');
const requireWu = createRequire(path.join(packageRoot, 'package.json'));
const { Command } = requireWu('commander');
const commandPath = path.join(packageRoot, 'dist/cli/messages.js');
const source = fs.readFileSync(commandPath, 'utf8')
  .replace(/^import .*;\n/gm, '')
  .replace('export function registerMessagesCommand', 'function registerMessagesCommand')
  .concat('\nglobalThis.register = registerMessagesCommand;');

async function send(args, fail = false) {
  const calls = [], output = [];
  const receipt = { key: { id: 'test-provider-message' }, messageTimestamp: 123 };
  const context = {
    loadConfig: () => ({ test: true }),
    withConnection: async (operation) => operation({ fake: true }),
    sendMedia: async (socket, jid, file, config, options) => {
      calls.push({ kind: 'media', jid, file, options });
      if (fail) throw Object.assign(new Error('upload failed'), { exitCode: 9 });
      return receipt;
    },
    sendText: async (socket, jid, text, config, options) => {
      calls.push({ kind: 'text', jid, text, options });
      return receipt;
    },
    console: { log: (line) => output.push(line), error: (line) => output.push(line) },
    process: { exit: (code) => { throw new Error(`exit ${code}`); } },
    EXIT_GENERAL_ERROR: 1,
    isAbsolute: path.isAbsolute,
    resolve: path.resolve,
    // Current wu routes CLI sends through its native action/IPC layer. Keep
    // that parser and action implementation real; only the network is fake.
    runAction: async (method, params, config) => context.actions[method]({ fake: true }, config, params),
  };
  vm.createContext(context);
  const actionPath = path.join(packageRoot, 'dist/core/actions.js');
  if (fs.existsSync(actionPath)) {
    const actions = fs.readFileSync(actionPath, 'utf8').replace(/^import .*;\n/gm, '')
      .replace(/^export /gm, '').concat('\nglobalThis.actions = actions;');
    vm.runInContext(actions, context, { filename: actionPath });
  }
  vm.runInContext(source, context, { filename: commandPath });
  const program = new Command();
  program.exitOverride();
  context.register(program);
  await program.parseAsync(['messages', 'send', 'recipient@s.whatsapp.net', ...args], { from: 'user' });
  return { calls: JSON.parse(JSON.stringify(calls)), output };
}

test('PDF attachment without body preserves path, caption, reply and provider receipt', async () => {
  const file = '/scoped/workdirs/attachments/Review pack.pdf';
  const result = await send(['--media', file, '--caption', 'Review “draft” — notes', '--reply-to', 'prior-message', '--json']);
  assert.deepEqual(result.calls, [{ kind: 'media', jid: 'recipient@s.whatsapp.net', file,
    options: { caption: 'Review “draft” — notes', replyTo: 'prior-message' } }]);
  assert.deepEqual(JSON.parse(result.output[0]), { id: 'test-provider-message', timestamp: 123 });
});

test('attachment text is the fallback caption', async () => {
  const result = await send(['Fallback caption', '--media', '/scoped/workdirs/review.pdf', '--json']);
  assert.equal(result.calls[0].kind, 'media');
  assert.equal(result.calls[0].options.caption, 'Fallback caption');
});

test('ordinary text send keeps its existing semantics', async () => {
  const result = await send(['Plain message', '--json']);
  assert.equal(result.calls[0].kind, 'text');
  assert.equal(result.calls[0].text, 'Plain message');
  assert.equal(JSON.parse(result.output[0]).id, 'test-provider-message');
});

test('failed upload remains a failure and is not retried', async () => {
  await assert.rejects(send(['--media', '/scoped/workdirs/review.pdf', '--json'], true), /exit 9/);
});

test('empty send is refused instead of creating a message', async () => {
  await assert.rejects(send(['--json']), /exit 1/);
});
