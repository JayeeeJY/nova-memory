#!/usr/bin/env python3
"""Offline frozen synthetic model-acceptance preparation; no execution or network."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import stat
import sys

ROOT = Path(__file__).resolve().parents[1]
CATALOG = 'eval/model-acceptance/'
CATALOG_NAMES = ('cases.json', 'review-checklist.json', 'acceptance-rules.json', 'README.md')
CATALOG_MANIFEST_SHA256 = '42ef1c11e976c9311d7965ee029812a14758a8d636f49355e16ce469ecd9e875'
SOURCE_NAMES = (
    'crates/nova-model-runtime/Cargo.toml',
    'crates/nova-model-runtime/src/lib.rs',
    'crates/nova-model-runtime/src/compiler.rs',
    'crates/nova-model-runtime/src/streaming_compiler.rs',
    'crates/nova-model-runtime/src/provider_capabilities.rs',
    'crates/nova-model-runtime/src/compiler_schema.rs',
    'crates/nova-model-runtime/src/quote_text_schema.rs',
    'crates/nova-model-runtime/src/generation_guard.rs',
    'scripts/model-acceptance.py', 'scripts/n2a-real-model-acceptance.py',
    'apps/nova-review/src/compiler-prompt.txt', 'apps/nova-review/src/compiler.rs', 'apps/nova-review/src/provider_capabilities.rs', 'crates/nova-model-runtime/src/compiler_metering.rs', 'crates/nova-core/src/intake/metering.rs', 'crates/nova-core/src/intake/capacity.rs', 'crates/nova-core/src/intake/compilation_capacity.rs',
    'apps/nova-review/src/intake.rs', 'crates/nova-core/src/intake/compiler.rs', 'crates/nova-core/src/intake/storage.rs',
    'crates/nova-core/src/intake/consolidation.rs',
)
PACKAGE_NAMES = frozenset((*CATALOG_NAMES, 'source-pins.json', 'HANDOFF.md', 'manifest.json'))
SCHEMA = 'nova.model-acceptance.preparation.v0.1'
LIMIT = 1024 * 1024
OBSERVATION_LIMIT = 65536
STATUSES = frozenset(('pass', 'fail', 'not_run'))


class PreparationError(ValueError):
    """Fixed safe messages only; never echo arbitrary input or credentials."""


def digest(data):
    return hashlib.sha256(data).hexdigest()


def canonical(value):
    return json.dumps(value, ensure_ascii=False, sort_keys=True, separators=(',', ':'), allow_nan=False).encode('utf-8')


def encoded(value):
    return json.dumps(value, ensure_ascii=False, sort_keys=True, indent=2, allow_nan=False).encode('utf-8') + b'\n'


def absolute(path):
    value = Path(path)
    if not value.is_absolute() or '..' in value.parts or value.name in ('', '.', '..'):
        raise PreparationError('An absolute non-aliased path is required')
    return value


def directory_fd(path):
    """Walk every ancestor using no-follow directory descriptors."""
    path = Path(path)
    if not path.is_absolute() or '..' in path.parts:
        raise PreparationError('An absolute non-aliased path is required')
    current = os.open('/', os.O_RDONLY | os.O_DIRECTORY)
    try:
        for component in path.parts[1:]:
            new = os.open(component, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW, dir_fd=current)
            os.close(current)
            current = new
        return current
    except OSError:
        os.close(current)
        raise PreparationError('Directory path must exist without symlinks') from None


def regular_bytes(path, limit=LIMIT, private=False):
    path = absolute(path)
    parent = directory_fd(path.parent)
    fd = -1
    try:
        fd = os.open(path.name, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK, dir_fd=parent)
        info = os.fstat(fd)
        if not stat.S_ISREG(info.st_mode) or info.st_size > limit:
            raise PreparationError('Selected input must be a bounded regular file')
        if private and (info.st_uid != os.getuid() or info.st_mode & 0o077):
            raise PreparationError('Selected report must be a private owned regular file')
        data = b''
        while len(data) <= limit:
            chunk = os.read(fd, min(65536, limit + 1 - len(data)))
            if not chunk:
                break
            data += chunk
        if len(data) > limit:
            raise PreparationError('Selected input exceeds the fixed byte limit')
        return data
    except OSError:
        raise PreparationError('Selected input is unavailable or is a symlink') from None
    finally:
        if fd >= 0:
            os.close(fd)
        os.close(parent)


def decode(data):
    def pairs(rows):
        output = {}
        for key, value in rows:
            if key in output:
                raise PreparationError('Duplicate JSON keys are not accepted')
            output[key] = value
        return output
    try:
        return json.loads(data, object_pairs_hook=pairs, parse_constant=lambda _: (_ for _ in ()).throw(PreparationError('Nonfinite JSON values are not accepted')))
    except (UnicodeError, json.JSONDecodeError, RecursionError):
        raise PreparationError('Selected JSON is invalid') from None


def write_new(path, data):
    if len(data) > LIMIT:
        raise PreparationError('Output exceeds the fixed byte limit')
    path = absolute(path)
    parent = directory_fd(path.parent)
    fd = -1
    try:
        fd = os.open(path.name, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o600, dir_fd=parent)
        with os.fdopen(fd, 'wb') as output:
            fd = -1
            output.write(data)
            output.flush()
            os.fsync(output.fileno())
    except OSError:
        raise PreparationError('Output must be new; no existing file is replaced') from None
    finally:
        if fd >= 0:
            os.close(fd)
        os.close(parent)


def fresh_target(path):
    """Do not inspect databases, credentials or any contents of existing profiles."""
    path = absolute(path)
    parent = directory_fd(path.parent)
    child = -1
    try:
        try:
            info = os.stat(path.name, dir_fd=parent, follow_symlinks=False)
        except FileNotFoundError:
            return 'absent'
        if not stat.S_ISDIR(info.st_mode) or info.st_uid != os.getuid() or info.st_mode & 0o077:
            raise PreparationError('Target must be absent or a private owned empty directory')
        child = os.open(path.name, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW, dir_fd=parent)
        opened = os.fstat(child)
        with os.scandir(child) as entries:
            nonempty = next(entries, None) is not None
        if (opened.st_dev, opened.st_ino) != (info.st_dev, info.st_ino) or nonempty:
            raise PreparationError('Nonempty or changed target is refused; no Vault was opened')
        return 'empty'
    except OSError:
        raise PreparationError('Target path is unavailable or aliased') from None
    finally:
        if child >= 0:
            os.close(child)
        os.close(parent)


def catalog(repo=ROOT):
    base = absolute(repo) / CATALOG
    raw = regular_bytes(base / 'dataset.manifest.json', 65536)
    if digest(raw) != CATALOG_MANIFEST_SHA256:
        raise PreparationError('Frozen catalog manifest changed; prepare a new reviewed version')
    manifest = decode(raw)
    if set(manifest) != {'schema_version', 'files'} or manifest['schema_version'] != 'nova.model-acceptance.catalog.v0.1' or set(manifest['files']) != set(CATALOG_NAMES):
        raise PreparationError('Catalog shape is invalid')
    files = {}
    for name in CATALOG_NAMES:
        data = regular_bytes(base / name, OBSERVATION_LIMIT)
        if manifest['files'][name] != {'sha256': digest(data), 'bytes': len(data)}:
            raise PreparationError('Frozen sample, checklist or rules changed')
        files[name] = data
    cases = decode(files['cases.json'])
    rows = cases['cases']
    if len(rows) != 12 or len({r['id'] for r in rows}) != 12 or sum(r['model_calls'] for r in rows) != 10:
        raise PreparationError('Frozen case counts or dispatch plan changed')
    return files, digest(raw), rows


def source_pins(repo=ROOT):
    return {name: {'sha256': digest(data), 'bytes': len(data)} for name in SOURCE_NAMES
            for data in (regular_bytes(absolute(repo) / name),)}


def handoff():
    return ("# Offline preparation; live execution blocked\n\n"
            "No service, Vault, credential, provider, model or account has been accessed.\n"
            "The planned ceiling is 10 model dispatches and zero automatic retries. It has NOT been applied to any runtime or monetary account.\n"
            "Before a future explicit run: verify fresh empty runtime identity, built artifact/source/prompt hashes, exact selected provider and model version, endpoint and processing consent, credential file, dispatch and currency limits, fixed case scope, bound correction/undo/withdraw targets, no concurrent writers, bounded transport, and independent Gold/reviewer.\n"
            "A model name is not proof of real inference. HTTP/schema/field checks are not semantic acceptance.\n"
            "This CLI deliberately has no execute command. Do not bypass it through the older three-case observation script. See the frozen README and review checklist.\n").encode('utf-8')


def prepare(output, target_profile, repo=ROOT):
    output, target = absolute(output), absolute(target_profile)
    if output == target or output in target.parents or target in output.parents:
        raise PreparationError('Package and fresh target must be separate nonoverlapping paths')
    state = fresh_target(target)
    files, catalog_hash, _ = catalog(repo)
    files.update({'source-pins.json': encoded(source_pins(repo)), 'HANDOFF.md': handoff()})
    body = {'schema_version': SCHEMA, 'catalog_sha256': catalog_hash,
            'target_profile': str(target), 'target_initial_state': state,
            'synthetic_inputs_only': True, 'network_requests': 0, 'model_calls': 0,
            'execute_supported': False, 'readiness': 'offline_prepared_live_execution_blocked',
            'planned_maximum_model_dispatches': 10, 'runtime_budget_applied': False,
            'independent_semantic_gold': False, 'semantic_acceptance': 'pending_independent_gold_and_review',
            'files': {name: {'sha256': digest(data), 'bytes': len(data)} for name, data in files.items()}}
    manifest = dict(body, plan_id=digest(canonical(body)))
    parent = directory_fd(output.parent)
    try:
        os.mkdir(output.name, mode=0o700, dir_fd=parent)
    except OSError:
        raise PreparationError('Package output must be a new directory') from None
    finally:
        os.close(parent)
    for name, data in files.items():
        write_new(output / name, data)
    # A partial failure never receives a valid final manifest.
    write_new(output / 'manifest.json', encoded(manifest))
    return manifest


def validate(package, repo=ROOT):
    package = absolute(package)
    fd = directory_fd(package)
    try:
        info = os.fstat(fd)
        names = set()
        with os.scandir(fd) as entries:
            for entry in entries:
                names.add(entry.name)
                if len(names) > len(PACKAGE_NAMES):
                    break
        if info.st_uid != os.getuid() or info.st_mode & 0o077 or names != PACKAGE_NAMES:
            raise PreparationError('Package must be private, complete and contain only fixed files')
    finally:
        os.close(fd)
    manifest = decode(regular_bytes(package / 'manifest.json', OBSERVATION_LIMIT, private=True))
    fixed = {'schema_version': SCHEMA, 'synthetic_inputs_only': True, 'network_requests': 0, 'model_calls': 0,
             'execute_supported': False, 'readiness': 'offline_prepared_live_execution_blocked',
             'planned_maximum_model_dispatches': 10, 'runtime_budget_applied': False,
             'independent_semantic_gold': False, 'semantic_acceptance': 'pending_independent_gold_and_review'}
    required = set(fixed) | {'catalog_sha256', 'target_profile', 'target_initial_state', 'files', 'plan_id'}
    if not isinstance(manifest, dict) or set(manifest) != required or any(type(manifest[k]) is not type(v) or manifest[k] != v for k, v in fixed.items()):
        raise PreparationError('Preparation manifest has invalid status, schema or budget')
    body = {key: value for key, value in manifest.items() if key != 'plan_id'}
    if manifest['plan_id'] != digest(canonical(body)) or manifest['target_initial_state'] not in ('absent', 'empty'):
        raise PreparationError('Preparation manifest identity is invalid')
    absolute(manifest['target_profile'])
    files, catalog_hash, _ = catalog(repo)
    if manifest['catalog_sha256'] != catalog_hash or not isinstance(manifest['files'], dict) or set(manifest['files']) != PACKAGE_NAMES - {'manifest.json'}:
        raise PreparationError('Preparation catalog or file set changed')
    expected = dict(files, **{'source-pins.json': encoded(source_pins(repo)), 'HANDOFF.md': handoff()})
    for name, wanted in expected.items():
        data = regular_bytes(package / name, private=True)
        if data != wanted or manifest['files'][name] != {'sha256': digest(data), 'bytes': len(data)}:
            raise PreparationError('Frozen package or current code changed; no silent update allowed')
    return manifest


def report_destination(output, package, manifest):
    output = absolute(output)
    for protected in (absolute(package), absolute(manifest['target_profile'])):
        if output == protected or protected in output.parents:
            raise PreparationError('Reports must stay outside the frozen package and fresh target')
    return output


def preflight(package, output, repo=ROOT):
    manifest = validate(package, repo)
    output = report_destination(output, package, manifest)
    state = fresh_target(manifest['target_profile'])
    result = {'schema_version': 'nova.model-acceptance.offline-preflight.v0.1', 'plan_id': manifest['plan_id'],
              'fresh_target_state': state, 'source_and_catalog_pins_valid': True,
              'network_requests': 0, 'model_calls': 0, 'vaults_opened': 0,
              'planned_maximum_model_dispatches': 10, 'runtime_budget_applied': False,
              'live_execution_ready': False, 'semantic_acceptance': 'pending_independent_gold_and_review',
              'remaining_gates': ['runtime_identity_and_empty_inventory', 'actual_build_and_model_version',
                                  'selected_endpoint_and_processing_consent', 'dispatch_and_currency_budget',
                                  'bounded_execution_transport', 'independent_gold_and_reviewer']}
    write_new(output, encoded(result))
    return result


def summarize(package, observations, output, repo=ROOT):
    manifest = validate(package, repo)
    output = report_destination(output, package, manifest)
    value = decode(regular_bytes(observations, OBSERVATION_LIMIT, private=True))
    _, _, cases = catalog(repo)
    allowed = {case['id'] for case in cases}
    if not isinstance(value, dict) or set(value) != {'schema_version', 'plan_id', 'cases'} or value['schema_version'] != 'nova.model-acceptance.observations.v0.1' or value['plan_id'] != manifest['plan_id'] or not isinstance(value['cases'], list) or len(value['cases']) > 12:
        raise PreparationError('Observation schema, plan or bound is invalid')
    seen, states = set(), []
    for row in value['cases']:
        if not isinstance(row, dict) or set(row) != {'id', 'transport', 'schema', 'fields'} or not isinstance(row['id'], str) or row['id'] not in allowed or row['id'] in seen:
            raise PreparationError('Observation case identity or fields are invalid')
        if any(not isinstance(row[k], str) or row[k] not in STATUSES for k in ('transport', 'schema', 'fields')):
            raise PreparationError('Observation status is invalid')
        seen.add(row['id'])
        states.extend(row[k] for k in ('transport', 'schema', 'fields'))
    result = {'schema_version': 'nova.model-acceptance.summary.v0.1', 'plan_id': manifest['plan_id'],
              'observed_cases': len(seen), 'required_cases': 12,
              'mechanical_observations': 'failed' if 'fail' in states else 'observed' if seen == allowed and all(s == 'pass' for s in states) else 'incomplete',
              'provider_identity_verified': False, 'independent_semantic_gold': False,
              'semantic_acceptance': 'pending_independent_gold_and_review', 'live_model_acceptance': False}
    write_new(output, encoded(result))
    return result


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest='command', required=True)
    p = commands.add_parser('prepare', help='Freeze code, prompt, fixtures and a planned budget; offline only')
    p.add_argument('--output', required=True, type=Path)
    p.add_argument('--target-profile', required=True, type=Path)
    for name in ('validate', 'preflight', 'summarize'):
        p = commands.add_parser(name)
        p.add_argument('--package', required=True, type=Path)
        if name != 'validate':
            p.add_argument('--output', required=True, type=Path)
        if name == 'summarize':
            p.add_argument('--observations', required=True, type=Path)
    args = parser.parse_args(argv)
    try:
        if args.command == 'prepare':
            result = prepare(args.output, args.target_profile)
        elif args.command == 'validate':
            result = validate(args.package)
        elif args.command == 'preflight':
            result = preflight(args.package, args.output)
        else:
            result = summarize(args.package, args.observations, args.output)
        # Only fixed statuses and a digest are printed, never arbitrary inputs or report content.
        print(json.dumps({'status': 'offline_' + args.command + '_complete', 'plan_id': result['plan_id'],
                          'live_model_acceptance': False, 'semantic_acceptance': 'pending_independent_gold_and_review'}))
        return 0
    except (PreparationError, OSError, ValueError, TypeError, KeyError, OverflowError):
        print('Offline preparation refused: check fixed files, new private paths and input schema. No model was called.', file=sys.stderr)
        return 1


if __name__ == '__main__':
    raise SystemExit(main())
