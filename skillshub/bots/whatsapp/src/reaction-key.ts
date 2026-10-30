/** Preserve stored linked-device keys on native IPC reactions. All other
 * actions use the original wu socket and upstream constraint/rate checks. */
export function ipcSocketWithReactionKeys(
  sock: any,
  getMessage: (id: string) => any,
  deserialize: (raw: any) => any,
): any {
  if (!sock) return undefined;
  return new Proxy(sock, {
    get(target, property) {
      if (property !== 'sendMessage') {
        const value = Reflect.get(target, property, target);
        return typeof value === 'function' ? value.bind(target) : value;
      }
      return (jid: string, content: any, options: any) => {
        if (content?.react?.key?.id) {
          const id = content.react.key.id;
          const stored = getMessage(id);
          if (stored?.chat_jid && stored.chat_jid !== jid) throw new Error('Reaction message belongs to another chat');
          if (stored?.raw) {
            let key;
            try { key = deserialize(stored.raw)?.key; } catch { /* Upstream minimal-key fallback. */ }
            if (key && typeof key === 'object') content = { ...content, react: { ...content.react,
              key: { ...key, remoteJid:jid, id, fromMe:stored.is_from_me === 1 } } };
          }
        }
        return target.sendMessage(jid, content, options);
      };
    },
  });
}
