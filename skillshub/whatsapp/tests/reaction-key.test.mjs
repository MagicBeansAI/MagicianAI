import test from 'node:test';
import assert from 'node:assert/strict';
import { ipcSocketWithReactionKeys } from '../../bots/whatsapp/src/reaction-key.ts';

test('native IPC preserves the linked-device participant key and original socket binding', async () => {
  const calls=[];
  const sock={ marker:'original', sendMessage(jid,content) { calls.push({jid,content,owner:this.marker}); } };
  const proxy=ipcSocketWithReactionKeys(sock,()=>({chat_jid:'group@g.us',is_from_me:0,raw:'stored'}),()=>({key:{participant:'member@lid',remoteJid:'old',id:'old',fromMe:true}}));
  await proxy.sendMessage('group@g.us',{react:{text:'👍',key:{id:'message'}}});
  assert.deepEqual(calls,[{jid:'group@g.us',content:{react:{text:'👍',key:{participant:'member@lid',remoteJid:'group@g.us',id:'message',fromMe:false}}},owner:'original'}]);
});
test('native IPC rejects a reaction targeting a stored message in another chat', () => {
  const proxy=ipcSocketWithReactionKeys({sendMessage(){throw new Error('must not send');}},()=>({chat_jid:'other@g.us'}),()=>({}));
  assert.throws(()=>proxy.sendMessage('group@g.us',{react:{text:'👍',key:{id:'message'}}}),/another chat/);
});
test('ordinary native messages pass through unchanged', () => {
  const message={text:'Hello'};let seen;
  const proxy=ipcSocketWithReactionKeys({sendMessage(_jid,content){seen=content;}},()=>{throw new Error('not a reaction');},()=>({}));
  proxy.sendMessage('recipient',message);
  assert.equal(seen,message);
});
