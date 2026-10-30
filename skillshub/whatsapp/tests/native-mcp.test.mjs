import assert from 'node:assert/strict';
import { mkdtempSync, writeFileSync, rmSync, copyFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import { pathToFileURL } from 'node:url';
import { createServer } from 'node:net';
import { execFile } from 'node:child_process';
import { promisify } from 'node:util';
import { fileURLToPath } from 'node:url';
import test from 'node:test';

const packageRoot = process.env.WU_TEST_PACKAGE_ROOT;

test('upstream stdio MCP discovers native schemas and reuses the existing daemon for writes', { skip:!packageRoot, timeout:30000 }, async () => {
  const root = resolve(packageRoot);
  const modules = resolve(root, '../..');
  const { Client } = await import(pathToFileURL(join(modules, '@modelcontextprotocol/sdk/dist/esm/client/index.js')));
  const { StdioClientTransport } = await import(pathToFileURL(join(modules, '@modelcontextprotocol/sdk/dist/esm/client/stdio.js')));
  const home = mkdtempSync(join(tmpdir(), 'wu-mcp-offline-'));
  // The parent owns the fake daemon lock: a real WhatsApp login is impossible.
  writeFileSync(join(home, 'wu.lock'), String(process.pid));
  writeFileSync(join(home, 'config.yaml'), 'constraints:\n  default: full\n');
  const calls = [];
  const server = createServer(conn => {
    let buffer = '';
    conn.on('data', chunk => {
      buffer += chunk;
      let end;
      while ((end = buffer.indexOf('\n')) >= 0) {
        const request = JSON.parse(buffer.slice(0,end)); buffer=buffer.slice(end+1);
        calls.push(request);
        conn.write(JSON.stringify({id:request.id,ok:true,result:request.method==='ping' ? {pong:true} : {id:'offline-provider-id',timestamp:123}})+'\n');
      }
    });
  });
  // Upstream's scoped daemon socket, never a user's live runtime socket.
  await new Promise((done, reject) => server.once('error',reject).listen(join(home,'daemon.sock'),done));
  const transport = new StdioClientTransport({ command:process.execPath, args:[join(root,'dist/cli/index.js'),'mcp'],
    env:{ PATH:process.env.PATH, WU_HOME:home, HOME:home }, stderr:'pipe' });
  let stderr='';
  transport.stderr?.on('data', chunk => stderr+=chunk);
  const client = new Client({name:'magician-offline-test',version:'1'});
  try {
    await client.connect(transport);
    const { tools } = await client.listTools();
    assert.ok(tools.length >= 34);
    const send=tools.find(tool=>tool.name==='wu_messages_send');
    assert.ok(send.inputSchema.properties.media_path);
    assert.equal(send.inputSchema.properties.poll,undefined);
    const result=await client.callTool({name:'wu_messages_send',arguments:{to:'offline@s.whatsapp.net',message:'offline proof'}});
    assert.equal(result.isError,undefined);
    assert.ok(JSON.stringify(result).includes('offline-provider-id'));
    assert.ok(calls.some(call=>call.method==='messages.send' && call.params.text==='offline proof'));
    assert.match(stderr,/Daemon is running/);
    // Ordinary CLI actions also run from a relocated snapshot. The npm
    // entrypoint must keep its relative modules; arguments reach upstream intact.
    const cliAdapter=join(home,'cli-adapter.mjs');
    copyFileSync(fileURLToPath(new URL('../bin/whatsapp-cli-adapter',import.meta.url)),cliAdapter);
    const cliEnv={PATH:`${join(modules,'.bin')}:${process.env.PATH}`,WU_HOME:home,HOME:home};
    const cliText='Literal comma, quotes " and --flags';
    const cliSent=await promisify(execFile)(process.execPath,[cliAdapter,'messages','send','offline@s.whatsapp.net',cliText,'--json'],{
      env:cliEnv,timeout:10000
    });
    assert.equal(JSON.parse(cliSent.stdout).id,'offline-provider-id');
    assert.ok(calls.some(call=>call.method==='messages.send' && call.params.text===cliText));
    await assert.rejects(promisify(execFile)(process.execPath,[cliAdapter,'definitely-not-a-wu-command'],{
      env:cliEnv,timeout:10000
    }),error=>error.code===1 && /unknown command/.test(error.stderr));
    // Simulate the relocated, snapshotted adapter: it must resolve upstream
    // through the reviewed PATH rather than adjacent node_modules.
    const pollAdapter=join(home,'poll-adapter.mjs');
    copyFileSync(fileURLToPath(new URL('../bin/whatsapp-poll-adapter',import.meta.url)),pollAdapter);
    const sent=await promisify(execFile)(process.execPath,[pollAdapter,'offline@s.whatsapp.net','--question','Choose','--option','A, with comma','--option','B'],{
      env:{PATH:`${join(modules,'.bin')}:${process.env.PATH}`,WU_HOME:home,HOME:home},timeout:10000
    });
    assert.equal(JSON.parse(sent.stdout).id,'offline-provider-id');
    assert.ok(calls.some(call=>call.method==='messages.send' && call.params.poll==='Choose' && call.params.options[0]==='A, with comma'));
    const governed=await promisify(execFile)(process.execPath,[pollAdapter,'offline@s.whatsapp.net','--question','Governed choice','--options','A, with comma','--literal-choice'],{
      env:{PATH:`${join(modules,'.bin')}:${process.env.PATH}`,WU_HOME:home,HOME:home},timeout:10000
    });
    assert.equal(JSON.parse(governed.stdout).id,'offline-provider-id');
    assert.ok(calls.some(call=>call.method==='messages.send' && call.params.poll==='Governed choice' && JSON.stringify(call.params.options)===JSON.stringify(['A, with comma','--literal-choice'])));
    await assert.rejects(promisify(execFile)(process.execPath,[pollAdapter,'offline@s.whatsapp.net','--question','Governed choice','--options','Only one'],{
      env:{PATH:`${join(modules,'.bin')}:${process.env.PATH}`,WU_HOME:home,HOME:home},timeout:10000
    }),/2–12/);
    await assert.rejects(promisify(execFile)(process.execPath,[pollAdapter,'offline@s.whatsapp.net','--question','Choose','--option','Only one'],{
      env:{PATH:`${join(modules,'.bin')}:${process.env.PATH}`,WU_HOME:home,HOME:home},timeout:10000
    }),/2–12/);
  } finally {
    await client.close();
    await new Promise(done=>server.close(done));
    rmSync(home,{recursive:true,force:true});
  }
});
