#!/usr/bin/env python3
"""Prepare private, body-blind Channel Assist review rows and freeze reviewed gold.

Suggested annotation labels are never gold by themselves. Reviewers explicitly
approve/correct all five required heads. Real rows must remain under runtime/evals.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import random
import time

REPO = Path(__file__).resolve().parents[1]
PROJECTION_VERSION = 'channel_classify_projection/1'
VOCAB = {
    'lane': ('needs_reply', 'follow_up', 'fyi', 'no_action'),
    'follow_up_kind': ('needs_reply', 'owner_owes', 'other_owes', 'waiting_on', 'check_back', 'schedule', 'none'),
    'action_owner': ('owner', 'counterparty', 'agent', 'unknown'),
    'urgency': ('low', 'normal', 'high'),
}
REQUIRED = (*VOCAB, 'repeat_of_recently_handled')


def private_path(path, runtime_root):
    path, root = Path(path).resolve(), Path(runtime_root).resolve()
    if path.is_relative_to(REPO) or not path.is_relative_to(root / 'evals' / 'channel_classify'):
        raise ValueError('Real review data must be outside git under runtime/evals/channel_classify')
    return path


def write_private(path, rows, root):
    path = private_path(path, root)
    path.parent.mkdir(parents=True, exist_ok=True, mode=0o700)
    # Exclusive creation prevents accidentally replacing a completed review.
    with path.open('x', encoding='utf-8') as output:
        os.chmod(path, 0o600)
        for row in rows:
            output.write(json.dumps(row, ensure_ascii=False, allow_nan=False) + '\n')
    return path


def freeze(rows):
    seen, gold = set(), []
    for row in rows:
        if row.get('review', {}).get('status') != 'approved':
            raise ValueError('Every frozen row needs explicit approved human review')
        review = row['review']
        if review.get('mode') == 'delegated' and not review.get('authorization_ref'):
            raise ValueError('Delegated adjudication requires explicit owner authorization provenance')
        if row['id'] in seen:
            raise ValueError('Duplicate reviewed case identity')
        seen.add(row['id'])
        expected = row.get('expected') or {}
        if any(expected.get(q) not in values for q, values in VOCAB.items()):
            raise ValueError('All closed heads must be reviewed')
        if type(expected.get('repeat_of_recently_handled')) is not bool:
            raise ValueError('Repeat status requires an explicit reviewed boolean')
        if row.get('projection_version') != PROJECTION_VERSION or not isinstance(row.get('input', {}).get('state'), dict):
            raise ValueError('Invalid or incompatible reviewed projection')
        gold.append(row)
    if not gold:
        raise ValueError('Empty review cannot become gold')
    return gold


def review_counts(rows):
    """Owner-authorized assistant judgments must not be reported as human labels."""
    approved = [row for row in rows if row.get('review', {}).get('status') == 'approved']
    delegated = sum(row['review'].get('mode') == 'delegated' for row in approved)
    return {'human_reviewed': len(approved) - delegated, 'delegated_reviewed': delegated,
            'approved_cases': len(approved)}


def render(rows, baselines=None):
    """A readable private packet; suggestions still require owner approval."""
    baselines = baselines or {}
    lines = ['# Channel classification review', '',
        'Review all five proposed heads for each case. Correct any that are wrong. '
        'Approving this packet approves these proposals, including repeat status; '
        'stored workflow state alone never counts as review.', '',
        'Lane: needs_reply / follow_up / fyi / no_action. Follow-up: needs_reply / '
        'owner_owes / other_owes / waiting_on / check_back / schedule / none. '
        'Owner: owner / counterparty / agent / unknown. Urgency: low / normal / high.', '']
    def safe(value):
        value=str(value).replace('\\','\\\\').replace('\r','').replace('\n',' ').replace('<','&lt;').replace('>','&gt;')
        for token in ('`','!','[',']','*','_','#','|'):
            value=value.replace(token,'\\'+token)
        return value
    for index, row in enumerate(rows,1):
        state=row['input']['state'];baseline=baselines.get(row['id'],{})
        suggestions=baseline.get('labels') or row.get('suggested',{})
        lines += [f'## {index}. {safe(state.get("subject","(none)"))}', '',
            f'Case: `{row["id"]}` ({row.get("partition","development")})', '',
            f'Sender: {safe(state.get("sender","unknown"))}; direction: {safe(state.get("latest_direction","unknown"))}', '',
            f'Summary: {safe(state.get("summary",""))}', '',
            f'Brief: {safe(state.get("safe_brief","none"))}', '',
            f'Follow-up hint: {safe(state.get("follow_up_hint","none"))}', '',
            f'Recent handled: {safe(state.get("recent_handled_followups","none"))}', '',
            '| Head | Proposal |', '| --- | --- |']
        for q in REQUIRED:
            value=suggestions.get(q)
            lines.append(f'| {q} | {safe(value) if value is not None else "REVIEW REQUIRED: choose a value"} |')
        lines += ['', 'Verdict: pending owner review', '']
    return '\n'.join(lines)


def write_markdown(path, text, root):
    path=private_path(path,root);path.parent.mkdir(parents=True,exist_ok=True,mode=0o700)
    with path.open('x',encoding='utf-8') as output:
        os.chmod(path,0o600);output.write(text)
    return path


def age(ts, now):
    if not ts or ts < 0:
        return 'unknown'
    days = max(0, now - ts) // 86_400_000
    return 'today' if days == 0 else ('1 day ago' if days == 1 else f'{days} days ago')


def project(row, now, handled):
    name, address = row.get('latest_from_name'), row.get('latest_from_address')
    sender = f'{name} <{address}>' if name and address else (name or address or '(unknown)')
    def list_text(key):
        values = json.loads(row.get(key) or '[]')
        return ', '.join(values) if values else '(none)'
    return {
        'channel': 'email' if row['provider'] in ('gmail', 'agentmail') else 'chat',
        'lane': "the agent's (Presto's)" if row['lane'] == 'envoy' else "the owner's",
        'subject': row.get('subject') or '(none)', 'sender': sender,
        'recipient_domains': list_text('recipient_domains_json'), 'label_ids': list_text('label_ids_json'),
        'message_count': str(row['message_count']), 'age': age(row.get('last_message_at'), now),
        'latest_message_id': row['message_id'], 'latest_message_age': age(row['internal_date'], now),
        'latest_direction': row.get('direction') or 'unknown', 'latest_intent': row.get('intent') or 'unknown',
        'needs_reply_hint': str(bool(row.get('needs_reply_hint'))).lower(),
        'follow_up_hint': row.get('follow_up_hint_json') or 'none',
        'recent_handled_followups': json.dumps(handled) if handled else 'none',
        'summary': (row.get('summary') or '')[:900], 'safe_brief': row.get('distill_brief_json') or 'none',
    }


def prepare(db, principal, workspace, count, seed):
    import duckdb
    now = int(time.time() * 1000)
    # Composite identities keep different providers/accounts/workspaces apart.
    with duckdb.connect(str(db), read_only=True) as connection:
        cursor = connection.execute('''
            WITH annotations AS (
              SELECT *, row_number() OVER (PARTITION BY principal,workspace,provider,account_alias,thread_id ORDER BY updated_at DESC,id) rank
              FROM mail_annotations WHERE principal=? AND workspace=?
            ), messages AS (
              SELECT *, row_number() OVER (PARTITION BY principal,workspace,provider,account_alias,thread_id ORDER BY internal_date DESC,message_id) rank
              FROM mail_messages WHERE principal=? AND workspace=? AND distill_state='done' AND NOT sensitive_suppressed
            )
            SELECT t.*,m.message_id,m.internal_date,m.direction,m.intent,m.needs_reply_hint,m.summary,
              m.follow_up_hint_json,m.distill_brief_json,m.distill_revision,
              a.label suggested_lane,a.proposed_action_json suggested_action,a.state annotation_state
            FROM mail_threads t JOIN messages m USING(principal,workspace,provider,account_alias,thread_id)
            JOIN annotations a USING(principal,workspace,provider,account_alias,thread_id)
            WHERE t.principal=? AND t.workspace=? AND m.rank=1 AND a.rank=1 AND NOT t.sensitive_suppressed
              AND m.summary IS NOT NULL AND a.label IN ('needs_reply','follow_up','fyi','no_action')
        ''', [principal, workspace] * 3)
        columns = [column[0] for column in cursor.description]
        candidates = [dict(zip(columns, values)) for values in cursor.fetchall()]
        random.Random(seed).shuffle(candidates)
        buckets = {lane: [r for r in candidates if r['suggested_lane'] == lane] for lane in VOCAB['lane']}
        candidates = []
        while any(buckets.values()):
            for bucket in buckets.values():
                if bucket: candidates.append(bucket.pop())
        rows = []
        for source in candidates[:count]:
            handled_cursor = connection.execute('''SELECT state,label,reason,proposed_action_json,updated_at FROM mail_annotations
                WHERE principal=? AND workspace=? AND provider=? AND account_alias=? AND thread_id=?
                AND state IN ('acknowledged','dismissed','completed') AND updated_at>=? ORDER BY updated_at DESC LIMIT 3''',
                [principal, workspace, source['provider'], source['account_alias'], source['thread_id'], now - 14 * 86_400_000])
            handled = [dict(state=s,label=l,reason=r,proposed_action=json.loads(a) if a else None,handled=age(t,now)) for s,l,r,a,t in handled_cursor.fetchall()]
            identity = {key: source[key] for key in ('provider','account_alias','thread_id','message_id','distill_revision')}
            identity.update(principal=principal, workspace=workspace)
            case_id = hashlib.sha256(json.dumps(identity,sort_keys=True).encode()).hexdigest()
            action = json.loads(source.get('suggested_action') or '{}')
            rows.append({'schema_version':1,'id':case_id,'projection_version':PROJECTION_VERSION,'source':identity,'captured_at_ms':now,
                'input':{'state':project(source,now,handled),'context':{'owner':principal,'today':time.strftime('%Y-%m-%d',time.gmtime(now/1000))}},
                'partition':'held_out' if int(case_id[:8],16)%5==0 else 'development',
                'suggested':{'lane':source['suggested_lane'],**{q:action.get(q) for q in VOCAB if q != 'lane'}},
                'annotation_state':source['annotation_state'],'expected':{},'review':{'status':'pending'}})
    return rows


def main():
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--root',type=Path,default=Path(os.environ.get('MAGICIAN_ROOT_DIR','~/MagicianNotes')).expanduser())
    parser.add_argument('--db',type=Path);parser.add_argument('--principal',default='anonymous');parser.add_argument('--workspace',default='default')
    parser.add_argument('--count',type=int,default=150);parser.add_argument('--seed',type=int,default=1)
    parser.add_argument('--reviewed',type=Path,help='Freeze a fully reviewed JSONL packet instead of preparing one')
    parser.add_argument('--packet',type=Path,help='Render an existing packet for review without querying the database')
    parser.add_argument('--baseline',type=Path,help='Use frozen LLM baseline proposals in the Markdown review')
    parser.add_argument('--output',type=Path,required=True)
    args=parser.parse_args()
    if args.packet:
        rows=[json.loads(line) for line in args.packet.read_text().splitlines() if line.strip()]
        baselines={r['id']:r for r in (json.loads(line) for line in args.baseline.read_text().splitlines() if line.strip())} if args.baseline else {}
        for row in rows:
            if baselines:
                expected_hash=hashlib.sha256(json.dumps(row['input'],sort_keys=True,separators=(',',':'),ensure_ascii=False).encode()).hexdigest()
                if baselines.get(row['id'],{}).get('input_sha256') != expected_hash:
                    parser.error('baseline proposals do not match this review projection')
        destination=write_markdown(args.output,render(rows,baselines),args.root)
        print(f'{len(rows)} cases rendered for owner review -> {destination}')
        return
    if args.reviewed:
        rows=freeze([json.loads(line) for line in args.reviewed.read_text().splitlines() if line.strip()])
    else:
        if not 1 <= args.count <= 1000: parser.error('count must be in 1..1000')
        db=args.db or args.root/'scopes'/args.principal/args.workspace/'mail_assist'/'mail_assist.duckdb'
        rows=prepare(db,args.principal,args.workspace,args.count,args.seed)
    destination=write_private(args.output,rows,args.root)
    print(f'{len(rows)} private rows written to {destination}; labels are {"reviewed" if args.reviewed else "pending review"}')

if __name__=='__main__':main()
