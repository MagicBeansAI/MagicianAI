import { AppLiveCollection, compareAppTimestamps, scopedAppsClient } from '$lib/apps/appLiveCollection';
import type { AppEntityChangeBatch, AppQueryRequest, AppRecordProjection } from '../../../../../../sdk/typescript/src/types';

export interface TownSquareReaction { post_id: string; member_id: string; emoji: string; created_at: string; }

/** Domain fields/decoration only; pagination, live merge and recovery belong to the SDK. */
export function townSquareCollection(installationId: string, surfaceRevision: number,
  reactionsChanged: () => void) {
  const client = scopedAppsClient();
  const reactions = new Map<string, TownSquareReaction>();
  const knownPosts = new Set<string>();
  const abort = new AbortController();
  const reactionQuery: Omit<AppQueryRequest, 'predicate'> = {
    pagination: 'keyset',
    protocol_version: '1', source_installation_id: installationId, entity: 'reaction',
    select: ['reaction_id', 'post_id', 'member_id', 'emoji', 'created_at'], limit: 100, purpose: 'owner_http_query',
  };
  function store(rows: readonly AppRecordProjection[]) {
    for (const row of rows) reactions.set(row.record_id, row.fields as unknown as TownSquareReaction);
    reactionsChanged();
  }
  async function changes(batch: AppEntityChangeBatch) {
    const ids = [...new Set(batch.changes.filter((change) => change.entity === 'reaction').map((change) => change.record_id))];
    if (!ids.length) return;
    const page = await client.queryData(installationId, { ...reactionQuery,
      predicate: { root: 0, nodes: [{ kind: 'in', field: 'reaction_id', values: ids }] },
    }, { signal: abort.signal });
    for (const id of ids) reactions.delete(id);
    store(page.envelope.value.filter((row) => knownPosts.has(String(row.fields.post_id))));
  }
  const collection = new AppLiveCollection({
    queryData: client.queryData.bind(client),
    readEntityChanges: async (id, options) => {
      const batch = await client.readEntityChanges(id, options);
      // limit=1 is the initial head capture; it deliberately does not replay old changes.
      if (options.limit !== 1) {
        reactionTail = reactionTail.catch(() => undefined).then(() => changes(batch));
        await reactionTail;
      }
      if (batch.reset_required && options.limit !== 1) { knownPosts.clear(); reactions.clear(); }
      return batch;
    },
  }, {
    surfaceRevision, recordIdField: 'post_id', pageSize: 25,
    request: { protocol_version: '1', source_installation_id: installationId, entity: 'post',
      select: ['post_id', 'author_id', 'surface', 'group_id', 'post_type', 'body', 'parent_id', 'created_at'],
      predicate: { root: 0, nodes: [{ kind: 'compare', field: 'surface', operator: 'equal', value: 'feed' }] },
      order: [{ field: 'created_at', direction: 'descending' }], purpose: 'owner_http_query',
    },
    compare: (left, right) => {
      const timeOrder = compareAppTimestamps(String(right.fields.created_at), String(left.fields.created_at));
      return timeOrder || (left.record_id < right.record_id ? -1 : left.record_id > right.record_id ? 1 : 0);
    },
  });
  let reactionTail = Promise.resolve();
  return { collection, reactions,
    loadReactions(records: readonly AppRecordProjection[]) {
      reactionTail = reactionTail.catch(() => undefined).then(async () => {
        const missing = records.map((row) => row.record_id).filter((id) => !knownPosts.has(id));
        for (let offset = 0; offset < missing.length; offset += 25) {
          const ids = missing.slice(offset, offset + 25);
          const request: AppQueryRequest = { ...reactionQuery,
            predicate: { root: 0, nodes: [{ kind: 'in', field: 'post_id', values: ids }] } };
          const rows: AppRecordProjection[] = [];
          let cursor: string | undefined;
          do {
            const page = await client.queryData(installationId, { ...request, ...(cursor ? { cursor } : {}) }, { signal: abort.signal });
            rows.push(...page.envelope.value); cursor = page.next_cursor;
          } while (cursor);
          for (const id of ids) knownPosts.add(id);
          store(rows);
        }
      });
      return reactionTail;
    },
    dispose() { abort.abort(); collection.dispose(); },
  };
}
