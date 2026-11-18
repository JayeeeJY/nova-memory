#!/usr/bin/env python3
"""Bounded real-provider replay on an explicitly configured SYNTHETIC local workspace.
Reports transport/schema/field observations, not independently verified semantic quality.
No models/dependencies installed; never reads personal memory or accepts API keys as arguments.
"""
import argparse, datetime, ipaddress, json, os, stat, time, urllib.error, urllib.parse, urllib.request, uuid
from pathlib import Path


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--base-url', default='http://127.0.0.1:3042')
    parser.add_argument('--owner-token-file', type=Path, required=True)
    parser.add_argument('--output', type=Path, required=True)
    args = parser.parse_args()
    parsed = urllib.parse.urlsplit(args.base_url)
    if parsed.scheme != 'http' or parsed.username or parsed.password or parsed.path not in ('', '/') or parsed.query or parsed.fragment:
        raise SystemExit('Exact local HTTP workspace origin required')
    try:
        assert ipaddress.ip_address(parsed.hostname).is_loopback
    except (ValueError, AssertionError):
        raise SystemExit('Loopback IP required')
    meta = args.owner_token_file.lstat()
    if not stat.S_ISREG(meta.st_mode) or meta.st_mode & 0o077 or meta.st_uid != os.getuid():
        raise SystemExit('Owner credential must be a private owned regular file')
    token = args.owner_token_file.read_text().strip()
    base = args.base_url.rstrip('/')
    opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))

    def call(path, body=None):
        headers = {'X-Nova-Review': 'owner-review', 'Authorization': 'Bearer ' + token}
        if body is not None:
            headers.update({'Origin': base, 'Content-Type': 'application/json'})
        request = urllib.request.Request(base + path, data=None if body is None else json.dumps(body, ensure_ascii=False).encode(), headers=headers)
        try:
            with opener.open(request, timeout=10) as response:
                return json.load(response)
        except urllib.error.HTTPError as error:
            raise RuntimeError('Local workspace request rejected: HTTP ' + str(error.code)) from None

    session = call('/console/session')
    if session.get('demo') is not True:
        raise SystemExit('Only an independent synthetic workspace may run this replay')
    root = '/console/api/owner/intake'
    configured = call(root + '/configuration')
    selection = configured['configuration']['selection']
    if not configured.get('compiler_available') or selection['location'] == 'unconfigured':
        raise SystemExit('Configure a real model and explicitly enable the compiler first')
    if selection.get('model_id', '').lower().startswith(('synthetic', 'mock', 'fixture')):
        raise SystemExit('Synthetic fixture is NOT a real model; choose a real model in settings')
    if args.output.exists():
        raise SystemExit('Refusing to overwrite the output report')
    report = {'schema_version': 'nova.n2a-real-provider-observations.v0.1', 'created_at': datetime.datetime.now(datetime.timezone.utc).isoformat(), 'model_id': selection['model_id'], 'location': selection['location'], 'configuration_revision': configured['configuration']['revision'], 'synthetic_inputs_only': True, 'independent_semantic_gold': False, 'semantic_acceptance': 'pending_independent_review', 'cases': []}
    fixtures = [
        ('negation_and_context', '合成验收材料：我在工作场景不参加超过30分钟的会议；私人聚会没有这个限制。'),
        ('plan_not_decision', '合成验收材料：我计划下个月学习钢琴，但还没有报名，日期也没有确定。'),
        ('unknown_not_invented', '合成验收材料：我还不知道自己偏好哪种会议形式，请不要替我作结论。'),
    ]
    try:
        for name, content in fixtures:
            receipt = call(root, {'schema_version': 'nova.intake.v0.1', 'request_id': 'n2a-real-' + str(uuid.uuid4()), 'source_platform': 'web-console', 'capture_fidelity': 'f2_visible_transcript', 'content': content, 'occurred_at': None})['receipt']
            deadline = time.monotonic() + 90
            while time.monotonic() < deadline:
                row = next((r for r in call(root + '/jobs')['items'] if r['job']['job_id'] == receipt['job_id']), None)
                if row and row['job']['state'] in ('ready', 'failed'):
                    break
                time.sleep(0.5)
            else:
                raise RuntimeError('Timed out; original remains saved, no inferred completion')
            memories = [r['memory']['unit'] for r in call(root + '/units')['items'] if any(e['source'] == receipt['source'] for e in r['memory']['unit']['evidence'])]
            report['cases'].append({'name': name, 'input': content, 'job': row['job'], 'units': memories, 'observations': {'state': row['job']['state'], 'unit_count': len(memories), 'polarity': [u['content']['polarity'] for u in memories], 'modality': [u['content']['modality'] for u in memories], 'conditions': [u['content']['conditions'] for u in memories], 'valid_time': [u['valid_time'] for u in memories]}, 'semantic_review': 'required'})
        report['transport_and_persistence_passed'] = all(c['job']['state'] == 'ready' for c in report['cases'])
    except Exception as error:
        report['transport_and_persistence_passed'] = False
        report['failure_type'] = type(error).__name__
    finally:
        with args.output.open('x') as output:
            json.dump(report, output, ensure_ascii=False, indent=2)
            output.write('\n')
    print('Bounded provider replay recorded. Semantic quality still requires independent review.')
    print('Report:', args.output)
    raise SystemExit(0 if report.get('transport_and_persistence_passed') else 1)


if __name__ == '__main__':
    main()
