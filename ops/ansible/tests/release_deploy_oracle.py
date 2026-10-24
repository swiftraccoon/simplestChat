"""Execute unchanged complete host deployment against finite owned external observations.

The real CLI, Runner, archive, flock, asset store, live-backup publication,
selection, rollback and post-unlock outcome remain in the reference path.
Only root/path/time/disk observations and inert Docker/curl children adapt.
No application, engine, network, database or host service is launched.
"""

from __future__ import annotations

import argparse
import fcntl
import hashlib
import io
import json
import os
import shutil
import signal
import stat
import sys
import tarfile
import tempfile
import uuid
from contextlib import ExitStack, redirect_stderr, redirect_stdout
from pathlib import Path
from subprocess import TimeoutExpired
from types import SimpleNamespace
from typing import TYPE_CHECKING, cast
from unittest.mock import patch

import release_fetch_controller_oracle as census
import release_stage_oracle as stage
from test_support import ROOT, obj, string

# isort: split
import bounded_process
import release_public as public
from release_json import JsonObject, JsonValue, decode_json

if TYPE_CHECKING:
    import subprocess
    from collections.abc import Sequence
    from typing import BinaryIO, Unpack

REVISION = stage.REVISION
OLD_REVISION = "b" * 40
OLD_IMAGE = "sha256:" + "d" * 64
UTC = stage.UTC
MAX_OUTPUT = 8 * 1024**2
MAX_FILE = 4 * 1024**2
WORKER_SECONDS = 60
OWN_PATHS = {
    "ops/ansible/tests/release_deploy_oracle.py",
    "ops/ansible/tests/test_release_deploy_oracle.py",
}
SCENARIOS = (
    "success",
    "prestaged",
    "received-hardlinks",
    "env-no-final-lf",
    "compose-crlf",
    "caddy-public-0644",
    "restore-without-read",
    "root-mode",
    "config-mode",
    "lock-busy",
    "current-unfinished",
    "release-unfinished",
    "archive-digest",
    "staged-manifest-diff",
    "images-syntax",
    "old-image-invalid",
    "old-revision-invalid",
    "already-selected",
    "old-image-label",
    "app-not-running",
    "app-image-drift",
    "app-config-drift",
    "db-config-drift",
    "proxy-config-drift",
    "db-unhealthy",
    "migrate-retained",
    "pre-loop-unready",
    "pre-public-unready",
    "ledger-missing",
    "ledger-extra",
    "ledger-checksum",
    "ledger-failed",
    "ledger-duplicate",
    "ledger-empty",
    "packaged-mismatch",
    "preview-unrelated",
    "runtime-loader",
    "compose-image-count",
    "env-image-duplicate",
    "legacy-assets",
    "asset-receipt-diff",
    "asset-mount-writable",
    "assets-old-create",
    "assets-old-copy",
    "assets-old-invalid",
    "assets-old-remove",
    "assets-new-copy",
    "assets-create-unknown",
    "quiet-no-token",
    "quiet-unavailable",
    "quiet-malformed",
    "quiet-active-zero",
    "quiet-active-deadline",
    "rooms-arrive-after-quiet",
    "db-size-invalid",
    "backup-disk-exact",
    "backup-disk-below",
    "dump-empty",
    "dump-nonzero",
    "dump-partial",
    "dump-above-inspection",
    "list-nonzero",
    "backup-file-sync",
    "backup-rename",
    "backup-directory-sync",
    "backup-receipt-write",
    "before-copy",
    "replace-journal-write",
    "replacement-observation-time",
    "app-stop",
    "select-compose-write",
    "select-env-write",
    "select-images-write",
    "select-assets",
    "candidate-up",
    "candidate-up-close",
    "candidate-up-close-rollback-up",
    "candidate-up-late-result",
    "candidate-wrong-image",
    "candidate-loop-unready",
    "candidate-public-unready",
    "database-restarted",
    "proxy-restarted",
    "rollback-stop",
    "rollback-compose-write",
    "rollback-assets",
    "rollback-up",
    "rollback-wrong-image",
    "rollback-loop-unready",
    "rollback-public-unready",
    "rollback-dependency",
    "complete-journal-write",
    "outcome-write",
    "outcome-file-sync",
    "outcome-directory-sync",
    "interrupt-before-stop",
    "interrupt-after-stop",
)
REFERENCE_PATHS = (
    "ops/ansible/files/release-public.py",
    "ops/ansible/files/release_public.py",
    "ops/ansible/files/release_artifact.py",
    "ops/ansible/files/release_json.py",
    "ops/ansible/files/release_assets.py",
    "ops/ansible/files/runtime_profile.py",
    "ops/ansible/files/bounded_process.py",
    "ops/ansible/tests/release_stage_oracle.py",
    "ops/ansible/tests/release_fetch_controller_oracle.py",
    "ops/ansible/tests/test_support.py",
)

# This fixed inert executable is intentionally self-contained: every original
# Runner call still creates a real bounded child, without importing the driver.
ADAPTER = r"""import hashlib,json,pathlib,re,sys
root=pathlib.Path(__file__).resolve().parent.parent
control=json.loads((root/'control.json').read_bytes())
path=root/'daemon.json'
s=json.loads(path.read_bytes())
a=sys.argv[1:]
output=b''
role=''
code=0
input_data=b''
sc=control['scenario']
new=control['newImage']
old=control['oldImage']
config=root/'config'
public=root/'public'

def container(name):
    ch={'simplestchat':'1','postgres':'2','caddy':'3'}[name]
    image=s['appImage'] if name=='simplestchat' else 'sha256:'+ch*64
    state={'Running':s['appRunning'] if name=='simplestchat' else True,
           'OOMKilled':False,'StartedAt':'2050-01-01T00:00:00Z'}
    if name=='postgres': state['Health']={'Status':'unhealthy' if sc=='db-unhealthy' else 'healthy'}
    restarts=1 if s['ups'] and ((sc=='database-restarted' and name=='postgres') or
                (sc=='proxy-restarted' and name=='caddy') or
                (sc=='rollback-dependency' and s['ups']>1 and name=='postgres')) else 0
    return {'id':ch*64,'image':image,'state':state,'restarts':restarts,'configHash':ch*64}

def image(selector):
    previous=selector==old
    return {'id':old if previous else new,'os':'linux','architecture':'amd64',
            'user':'10001:10001','labels':{'org.opencontainers.image.revision':
            ('e'*40 if sc=='old-image-label' else 'b'*40) if previous else 'a'*40},
            'cmd':['/app/simplestChat'],'entrypoint':None,'workingDir':'/app',
            'hasHealthcheck':False,'envNames':['PATH']}

def render(filename,envfile):
    text=filename.read_text()
    images=re.findall(r'image: "(sha256:[a-f0-9]{64})"',text)
    if len(images)!=2: images=[old,old]
    environment=dict(line.split('=',1) for line in (config/'app.env').read_text().splitlines()
                     if '=' in line)
    m=re.search(r'- "([^"\n]+/(?:candidate|reviewed)\.env)"',text)
    if m: environment.update(dict(line.split('=',1) for line in pathlib.Path(m[1]).read_text()
                                 .splitlines() if '=' in line))
    services={'postgres':{'image':'sha256:'+'2'*64},'caddy':{'image':'sha256:'+'3'*64}}
    for i,name in enumerate(('simplestchat','migrate')):
        services[name]={'image':images[i],'environment':environment if i==0 else
            {'RUN_MIGRATIONS':'true'},'user':'10001:10001','init':True,'read_only':True,
            'cap_drop':['ALL'],'security_opt':['no-new-privileges:true'],
            'volumes':[{'type':'bind','source':str(public/'postgres-socket'),
            'target':'/run/simplestchat-postgres','read_only':True}],
            'tmpfs':['/tmp:rw,nosuid,nodev,noexec,size='+('64m' if i==0 else '32m')+',mode=1777']}
    if '/run/simplestchat-browser-assets' in text:
        services['caddy']['volumes']=[{'type':'bind','source':str(public/'browser-assets'),
            'target':'/run/simplestchat-browser-assets','read_only':sc!='asset-mount-writable',
            'bind':{'create_host_path':True}}]
    if filename!=config/'compose.public.yml' and sc=='preview-unrelated':
        services['simplestchat']['environment']['ALLOW_AD_HOC_ROOMS']='true'
    return {'services':services}

if a[:2]==['--host','unix:///var/run/docker.sock']:
    a=a[2:]
    if a[:2]==['image','load']:
        role='image-load';s['loaded']=True
    elif a[:2]==['image','inspect']:
        role='image-inspect';output=json.dumps(image(a[-1])).encode()
    elif a[0]=='compose':
        filename=pathlib.Path(a[a.index('-f')+1]);envfile=pathlib.Path(a[a.index('--env-file')+1])
        a=a[a.index('-f')+2:]
        if a[:2]==['--profile','maintenance']: a=a[2:]
        if a[:2]==['config','--hash']:
            name=a[-1];role='config-hash-'+name
            ch={'simplestchat':'1','postgres':'2','caddy':'3'}[name]
            mismatch={'app-config-drift':'simplestchat','db-config-drift':'postgres',
                      'proxy-config-drift':'caddy'}
            output=(name+' '+('9'*64 if mismatch.get(sc)==name else ch*64)+'\n').encode()
        elif a[0]=='config':
            role='compose-preview' if filename!=config/'compose.public.yml' else 'compose-config'
            output=json.dumps(render(filename,envfile)).encode()
        elif a[0]=='ps':
            name=a[-1];role='ps-'+name
            if name=='migrate': output=b'9'*64 if sc=='migrate-retained' else b''
            elif sc=='app-not-running' and name=='simplestchat': output=b''
            else: output=({'simplestchat':'1','postgres':'2','caddy':'3'}[name]*64).encode()
        elif a[0]=='stop':
            role='rollback-stop' if s['ups'] else 'app-stop'
            if sc==role or sc=='app-stop' and not s['stops']: code=7
            s['stops']+=1;s['appRunning']=False
        elif a[0]=='up':
            s['ups']+=1;role='candidate-up' if s['ups']==1 else 'rollback-up'
            if sc==role or sc=='candidate-up-close-rollback-up' and role=='rollback-up':code=7
            s['appRunning']=True
            selected=json.loads((config/'images.json').read_bytes())['serverImage']
            s['appImage']=old if sc=='candidate-wrong-image' and s['ups']==1 else selected
            if sc=='rollback-wrong-image' and s['ups']>1: s['appImage']=new
        elif a[0]=='logs': role='failure-logs';output=b'INERT_PRIVATE_CANDIDATE_LOG\n'
        else: raise ValueError('unknown closed Compose role')
    elif a[0]=='create':
        s['creates']+=1
        kind='asset' if 'exit 0' in a else 'validator'
        image_id=a[a.index('--entrypoint')+2]
        role=('assets-old-create' if image_id==old else 'assets-new-create') if kind=='asset' else \
             'validator-create'
        cid=format(100+s['creates'],'064x')
        s['containers'][cid]={'image':image_id,'kind':kind,'name':a[a.index('--name')+1]}
        if sc==role: code=7
        output=b'unknown' if sc=='assets-create-unknown' and kind=='asset' else cid.encode()
    elif a[0]=='cp':
        cid=a[1].split(':')[0]; previous=s['containers'][cid]['image']==old
        role='assets-old-copy' if previous else 'assets-new-copy'
        if sc==role: code=7
        output=(root/('assets-invalid.tar' if sc=='assets-old-invalid' and previous else
                     'assets-old.tar' if previous else 'assets-new.tar')).read_bytes()
    elif a[0] in ('start','wait','logs','rm'):
        cid=a[-1];item=s['containers'].get(cid)
        if a[0]=='rm' and item is None:
            cid=next((key for key,item in s['containers'].items() if item['name']==cid),cid)
            item=s['containers'].get(cid)
        if a[0]=='rm':
            role='assets-old-remove' if item and item['image']==old and item['kind']=='asset' else \
                 'container-remove'
            if sc==role: code=7
            if not code: s['containers'].pop(cid,None)
        elif a[0]=='logs':
            s['migrationLogs']+=1;role='migration-logs'
            checksum=('e'*96 if sc=='packaged-mismatch' and s['migrationLogs']>1
                      else control['checksum'])
            output=(checksum+'  /app/migrations/001_fixture.sql\n').encode()
        elif a[0]=='wait': role='validator-wait';output=b'0\n'
        else: role='validator-start'
    elif a[0]=='inspect':
        cid=a[-1];role='container-inspect'
        if cid in ('1'*64,'2'*64,'3'*64):
            name={'1'*64:'simplestchat','2'*64:'postgres','3'*64:'caddy'}[cid]
            data=container(name)
            if name=='simplestchat' and sc=='app-image-drift' and not s['ups']:
                data['image']='sha256:'+'9'*64
            output=json.dumps(data).encode()
        else: output=json.dumps({'image':s['appImage'],'state':{'Running':False}}).encode()
    elif a[0]=='exec':
        if 'pg_database_size' in ' '.join(a):
            role='db-size';output=b'invalid' if sc=='db-size-invalid' else b'8388608\n'
        elif 'psql' in a:
            role='db-ledger';line='1 t '+control['checksum']+'\n'
            if sc=='ledger-missing': line='2 t '+control['checksum']+'\n'
            if sc=='ledger-extra': line+='2 t '+'e'*96+'\n'
            if sc=='ledger-checksum': line='1 t '+'e'*96+'\n'
            if sc=='ledger-failed': line='1 f '+control['checksum']+'\n'
            if sc=='ledger-duplicate': line+=line
            if sc=='ledger-empty': line=''
            output=line.encode()
        elif 'pg_dump' in a:
            role='dump';output=(root/'dump.bin').read_bytes()
            if sc=='dump-empty':output=b''
            if sc in ('dump-nonzero','dump-partial'):code=7
            if sc=='dump-partial':output=output[:3]
        elif 'pg_restore' in a:
            role='restore-list'
            if sc!='restore-without-read':input_data=sys.stdin.buffer.read(4*1024*1024+1)
            if sc=='list-nonzero':code=7
            output=b'INERT_ARCHIVE_LIST\n'
        else: raise ValueError('unknown closed exec role')
    else: raise ValueError('unknown closed Docker role')
else:
    if a[-1].endswith('/metrics'):
        role='metrics';s['metrics']+=1
        if sc=='quiet-unavailable':code=7
        value=0
        if sc=='quiet-active-zero' and s['metrics']==1:value=2
        if sc=='quiet-active-deadline':value=2
        if sc=='rooms-arrive-after-quiet' and s['metrics']>1:value=3
        output=b'invalid' if sc=='quiet-malformed' else \
               ('simplestchat_rooms_active '+str(value)+'\n').encode()
    elif a[-1].endswith('/ready'):
        public_request=a[-1].startswith('https:')
        role=('public-' if public_request else 'loop-')+('pre' if not s['ups'] else
             'candidate' if s['ups']==1 else 'rollback')+'-ready'
        failure={'pre-loop-unready':'loop-pre-ready','pre-public-unready':'public-pre-ready',
                 'candidate-loop-unready':'loop-candidate-ready',
                 'candidate-public-unready':'public-candidate-ready',
                 'rollback-loop-unready':'loop-rollback-ready',
                 'rollback-public-unready':'public-rollback-ready'}
        if failure.get(sc)==role:code=7
        output=b'{"status":"ready"}'
    else:raise ValueError('unknown closed HTTP role')
if sc.startswith('rollback-') and s['ups']==1 and role=='loop-candidate-ready':code=7
s['calls'].append({'role':role,'argv':sys.argv[1:],'status':code,
                  'stdoutSha256':hashlib.sha256(output).hexdigest(),'stdoutBytes':len(output),
                  'inputSha256':hashlib.sha256(input_data).hexdigest(),
                  'inputBytes':len(input_data)})
path.write_text(json.dumps(s,sort_keys=True))
if code:sys.stderr.buffer.write(b'INERT_EXTERNAL_FAILURE\n')
sys.stdout.buffer.write(output)
sys.stdout.buffer.flush()
sys.exit(code)
"""


def encoded(value: object) -> bytes:
    """Keep bounded ordinary JSON in a deterministic fixture encoding."""
    return json.dumps(value, separators=(",", ":"), sort_keys=True).encode()


def normalized(value: JsonValue, root: Path) -> JsonValue:
    """Normalize only owned paths/entropy and known timestamp fields."""
    wall_fields = {
        "startedAt",
        "finishedAt",
        "stagedAt",
        "completedAt",
        "interruptionStartedAt",
        "interruptionFinishedAt",
        "roomsObservedBeforeStopAt",
    }
    if isinstance(value, dict):
        return {
            key: "$UTC" if key in wall_fields and isinstance(item, str) else normalized(item, root)
            for key, item in value.items()
        }
    if isinstance(value, list):
        return [normalized(item, root) for item in value]
    return stage.normalized(value, root) if isinstance(value, str) else value


def asset_archive(name: str, data: bytes) -> bytes:
    """Create real directory TAR input for the unchanged asset validator."""
    output = io.BytesIO()
    with tarfile.open(fileobj=output, mode="w") as archive:
        member = tarfile.TarInfo("assets/" + name)
        member.size = len(data)
        archive.addfile(member, io.BytesIO(data))
    return output.getvalue()


def fixture(root: Path, scenario: str) -> JsonObject:  # noqa: C901, PLR0912, PLR0915 -- Fixtures.
    """Export pristine equal native/reference host inputs and closed daemon observations."""
    base, manifest, archive = stage.fixture("success")
    new_image = string(obj(decode_json(string(obj(base, "daemon"), "inspect"))), "id")
    control: JsonObject = {
        "scenario": scenario,
        "revision": REVISION,
        "newImage": new_image,
        "oldImage": OLD_IMAGE,
        "quietSeconds": 10
        if scenario.startswith("quiet-") or scenario == "rooms-arrive-after-quiet"
        else 0,
        "freeBytes": 20 * 1024**3,
        "checksum": stage.CHECKSUM,
    }
    if scenario in {"backup-disk-exact", "backup-disk-below"}:
        control["freeBytes"] = 1024**3 + 2 * 8388608 - (scenario == "backup-disk-below")
    for name in ("public", "config", "work", "bin", "responses"):
        stage.directory(root / name)
    for name in ("releases", "results"):
        stage.directory(root / "public" / name)
    selected = root / "public/releases" / REVISION
    stage.directory(selected)
    stage.write(selected / "image.tar", archive)
    stage.write(selected / "release.json", manifest)
    stage.write(root / "work/workload.lock", b"")
    stage.write(root / "control.json", encoded(control))
    compose = (
        f'services:\n  simplestchat:\n    image: "{OLD_IMAGE}"\n'
        "    env_file:\n      - ./app.env\n"
        f'  migrate:\n    image: "{OLD_IMAGE}"\n'
        "  caddy:\n    volumes:\n"
        f'      - "{root}/public/browser-assets:/run/simplestchat-browser-assets:ro"\n'
    )
    environment = (
        f"SIMPLESTCHAT_IMAGE={OLD_IMAGE}\nRUN_MIGRATIONS=false\n"
        "WEBAUTHN_ORIGIN=https://fixture.invalid\n"
    )
    if scenario != "quiet-no-token":
        environment += "METRICS_TOKEN=INERT_fixture_metrics_token_0123456789\n"
    if scenario == "runtime-loader":
        environment += "LD_PRELOAD=INERT\n"
    if scenario == "env-no-final-lf":
        environment = environment.rstrip("\n")
    if scenario == "env-image-duplicate":
        environment += f"SIMPLESTCHAT_IMAGE={OLD_IMAGE}\n"
    if scenario == "compose-image-count":
        compose += f'  extra:\n    image: "{OLD_IMAGE}"\n'
    if scenario == "legacy-assets":
        compose = compose.partition("  caddy:")[0]
    if scenario == "compose-crlf":
        compose = compose.replace("\n", "\r\n")
    stage.write(root / "config/compose.public.yml", compose.encode())
    stage.write(root / "config/app.env", environment.encode())
    images: JsonObject = {"revision": OLD_REVISION, "serverImage": OLD_IMAGE}
    if scenario == "old-image-invalid":
        images["serverImage"] = "wrong"
    if scenario == "old-revision-invalid":
        images["revision"] = "wrong"
    if scenario == "already-selected":
        images.update(revision=REVISION, serverImage=new_image)
    stage.write(
        root / "config/images.json", b"{" if scenario == "images-syntax" else encoded(images)
    )
    caddy = b"INERT public asset proxy\n"
    stage.write(
        root / "config/Caddyfile", caddy, 0o644 if scenario == "caddy-public-0644" else 0o600
    )
    receipt: JsonObject = {"schemaVersion": 1, "caddyfileSha256": hashlib.sha256(caddy).hexdigest()}
    if scenario == "asset-receipt-diff":
        receipt["caddyfileSha256"] = "e" * 64
    stage.write(root / "config/browser-assets-installed.json", encoded(receipt))
    for label, image in (("old", OLD_IMAGE), ("new", new_image)):
        stage.write(
            root / f"assets-{label}.tar", asset_archive(f"media-{label}123456.js", image.encode())
        )
    stage.write(root / "assets-invalid.tar", asset_archive("private.map", b"inert"))
    stage.write(
        root / "dump.bin",
        b"PGDMP-INERT-ONLY"
        + (b"z" * (2 * 1024**2) if scenario == "dump-above-inspection" else b""),
    )
    state: JsonObject = {
        "calls": [],
        "appImage": OLD_IMAGE,
        "appRunning": True,
        "ups": 0,
        "stops": 0,
        "creates": 0,
        "containers": {},
        "metrics": 0,
        "migrationLogs": 0,
        "loaded": False,
    }
    stage.write(root / "daemon.json", encoded(state))
    adapter = ("#!" + sys.executable + "\n" + ADAPTER).encode()
    stage.write(root / "bin/docker", adapter, 0o700)
    stage.write(root / "bin/curl", adapter, 0o700)
    if scenario in {"root-mode", "config-mode"}:
        (root / ("public" if scenario == "root-mode" else "config")).chmod(0o755)
    if scenario == "archive-digest":
        value = obj(decode_json(manifest))
        value["archiveSha256"] = "e" * 64
        _ = (selected / "release.json").write_bytes(encoded(value))
    if scenario in {"prestaged", "staged-manifest-diff"}:
        stage.write(
            selected / "staged.json",
            encoded(
                {
                    "schemaVersion": 1,
                    "revision": REVISION,
                    "serverImage": new_image,
                    "manifestSha256": hashlib.sha256(manifest).hexdigest()
                    if scenario == "prestaged"
                    else "e" * 64,
                    "stagedAt": UTC,
                }
            ),
        )
    if scenario == "received-hardlinks":
        stage.directory(root / "received")
        for name in ("image.tar", "release.json"):
            os.link(selected / name, root / "received" / name, follow_symlinks=False)
    if scenario in {"current-unfinished", "release-unfinished"}:
        stage.write(
            root
            / (
                "work/current.json"
                if scenario == "current-unfinished"
                else "public/release-state.json"
            ),
            encoded({"schemaVersion": 1, "finalized": False}),
        )
    return control


def python_pins() -> JsonObject:
    """Bind every maintained original, including the now-existing Stage oracle pair."""
    paths = {path for name in census.PYTHON_ROOTS for path in (ROOT / name).rglob("*.py")}
    paths.update(ROOT / name for name in census.PYTHON_VENDOR_PATHS)
    return {
        path.relative_to(ROOT).as_posix(): stage.pin(path)
        for path in sorted(paths)
        if path.relative_to(ROOT).as_posix() not in OWN_PATHS
    }


def baseline_pins(values: JsonObject) -> JsonObject:
    """Keep each exact author baseline path; aggregate counts cannot replace originals."""
    if any(name not in values for name in BASELINE_PYTHON_PATHS):
        message = "original deploy source missing"
        raise ValueError(message)
    return {name: values[name] for name in BASELINE_PYTHON_PATHS}


def inventory(path: Path, root: Path) -> JsonObject:
    """Measure full private trees, preserving raw bytes even when semantic JSON is available."""
    result: JsonObject = {}
    for member in sorted(path.rglob("*")):
        metadata = member.lstat()
        name = member.relative_to(path).as_posix()
        mode = stat.S_IMODE(metadata.st_mode)
        if stat.S_ISLNK(metadata.st_mode):
            result[name] = {"symlink": str(member.readlink()), "mode": mode}
        elif stat.S_ISDIR(metadata.st_mode):
            result[name] = {"directory": True, "mode": mode}
        elif stat.S_ISREG(metadata.st_mode):
            item = stage.pin(member)
            if member.suffix == ".json":
                try:
                    item["value"] = normalized(decode_json(member.read_bytes()), root)
                except ValueError:
                    item["text"] = member.read_text()
            elif member.name in {"validation-name.txt", "asset-container-name.txt"}:
                item["text"] = normalized(member.read_text(), root)
            result[name] = item
        else:
            message = "unexpected deploy fixture file kind"
            raise ValueError(message)
    return result


def identities(path: Path) -> JsonObject:
    """Keep original read-only inode generations separate from cross-fixture byte parity."""
    result: JsonObject = {}
    for member in sorted(path.rglob("*")):
        metadata = member.lstat()
        if stat.S_ISREG(metadata.st_mode):
            result[member.relative_to(path).as_posix()] = {
                "dev": metadata.st_dev,
                "ino": metadata.st_ino,
                "uid": metadata.st_uid,
                "gid": metadata.st_gid,
                "nlink": metadata.st_nlink,
                "mode": metadata.st_mode,
                "bytes": metadata.st_size,
                "mtimeNs": metadata.st_mtime_ns,
            }
    return result


def command_state(root: Path, attempt: Path) -> JsonObject:
    """Observe actual files at an external-effect boundary; final-state replay is insufficient."""
    selection: JsonObject = {}
    for name in public.SELECTION:
        path = root / "config" / name
        data = path.read_bytes()
        try:
            value = decode_json(data) if name.endswith(".json") else data.decode()
        except ValueError:
            value = data.decode()
        selection[name] = {
            "mode": stat.S_IMODE(path.lstat().st_mode),
            "value": normalized(value, root),
        }
    journal = root / "public/release-state.json"
    version = root / "public/browser-assets/public/app-version.json"
    dump, receipt = attempt / "database-before.dump", attempt / "database-before.receipt.json"
    return {
        "selection": selection,
        "journal": normalized(decode_json(journal.read_bytes()), root)
        if journal.exists()
        else None,
        "assetVersion": decode_json(version.read_bytes()) if version.exists() else None,
        "backup": stage.pin(dump) if dump.exists() else None,
        "backupReceipt": normalized(decode_json(receipt.read_bytes()), root)
        if receipt.exists()
        else None,
        "lockHeld": not stage.lock_available(root / "work/workload.lock"),
    }


def worker(root: Path) -> JsonObject:  # noqa: C901, PLR0915 -- One complete owned reference.
    """Run unchanged CLI/main and all policy helpers with actual inert child processes."""
    control = obj(decode_json((root / "control.json").read_bytes()))
    scenario = string(control, "scenario")
    output, error = io.StringIO(), io.StringIO()
    writes: list[JsonValue] = []
    atomic_intended: list[JsonValue] = []
    calls: list[JsonValue] = []
    children: list[tuple[subprocess.Popen[bytes], BinaryIO, BinaryIO]] = []
    atomic_original, fsync_original = public.atomic, os.fsync
    run_original, pump_original = public.Runner.run, bounded_process.pump
    replace_original, copy_original = Path.replace, shutil.copyfile
    active: list[Path] = []
    faulted = False
    tick = 0.0

    def monotonic() -> float:
        return tick

    def sleep(seconds: float) -> None:
        nonlocal tick
        tick += seconds

    def timestamp() -> str:
        nonlocal faulted
        if (
            scenario == "replacement-observation-time"
            and not faulted
            and (root / "public/results").exists()
            and any((root / "public/results").glob("release.*/replacement-metrics-curl.config"))
        ):
            faulted = True
            message = "INERT_REPLACEMENT_TIMESTAMP"
            raise OSError(message)
        return UTC

    def atomic(path: Path, data: JsonValue | bytes) -> None:
        nonlocal faulted
        phase = data.get("phase", "") if isinstance(data, dict) else ""
        replaced = obj(decode_json((root / "daemon.json").read_bytes()))["stops"] != 0
        selected = path.parent == root / "config"
        name = path.name
        faults = {
            "backup-receipt-write": name == "database-before.receipt.json",
            "replace-journal-write": name == "release-state.json"
            and phase == "replace_application",
            "select-compose-write": selected and name == "compose.public.yml" and replaced,
            "select-env-write": selected and name == "app.env" and replaced,
            "select-images-write": selected and name == "images.json" and replaced,
            "rollback-compose-write": selected
            and name == "compose.public.yml"
            and replaced
            and obj(decode_json((root / "daemon.json").read_bytes()))["ups"] == 1,
            "complete-journal-write": name == "release-state.json" and phase == "complete",
            "outcome-write": name == "outcome.json",
            "interrupt-before-stop": name == "release-state.json"
            and phase == "replace_application",
            "interrupt-after-stop": selected and name == "compose.public.yml" and replaced,
        }
        intended = (
            data
            if isinstance(data, bytes)
            else data.encode()
            if isinstance(data, str)
            else (json.dumps(data, indent=2) + "\n").encode()
        )
        atomic_intended.append(
            {
                "path": normalized(str(path), root),
                "sha256": hashlib.sha256(intended).hexdigest(),
                "bytes": len(intended),
                "value": normalized(data.decode() if isinstance(data, bytes) else data, root),
            }
        )
        active.append(path)
        try:
            if not faulted and faults.get(scenario, False):
                faulted = True
                if scenario.startswith("interrupt-"):
                    handler = signal.getsignal(signal.SIGINT)
                    if callable(handler):
                        handler(signal.SIGINT, None)
                message = "INERT_ATOMIC_WRITE"
                raise OSError(message)
            atomic_original(path, data)
            writes.append(
                {
                    "path": normalized(str(path), root),
                    "value": normalized(data.decode() if isinstance(data, bytes) else data, root),
                    "lockHeld": not stage.lock_available(root / "work/workload.lock"),
                }
            )
        finally:
            _ = active.pop()

    def fsync(descriptor: int) -> None:
        nonlocal faulted
        directory = stat.S_ISDIR(os.fstat(descriptor).st_mode)
        target = active[-1].name if active else ""
        # The backup publisher's two actual fsync calls happen outside atomic().
        backup = not active and any(
            (root / "public/results").glob("release.*/database-before.dump.partial")
        )
        faults = {
            "outcome-file-sync": target == "outcome.json" and not directory,
            "outcome-directory-sync": target == "outcome.json" and directory,
            "backup-file-sync": backup and not directory,
            "backup-directory-sync": not active
            and directory
            and any((root / "public/results").glob("release.*/database-before.dump")),
        }
        if not faulted and faults.get(scenario, False):
            faulted = True
            message = "INERT_FSYNC"
            raise OSError(message)
        fsync_original(descriptor)

    def replace(path: Path, destination: Path | str) -> Path:
        nonlocal faulted
        state = obj(decode_json((root / "daemon.json").read_bytes()))
        asset_select = Path(destination).name == "app-version.json"
        selected_fault = scenario == "select-assets" and state["ups"] == 0
        rollback_fault = scenario == "rollback-assets" and state["ups"] == 1
        if not faulted and asset_select and (selected_fault or rollback_fault):
            faulted = True
            message = "INERT_ASSET_SELECTION_RENAME"
            raise OSError(message)
        if scenario == "backup-rename" and not faulted and path.name.endswith(".dump.partial"):
            faulted = True
            message = "INERT_BACKUP_RENAME"
            raise OSError(message)
        return replace_original(path, destination)

    def copy(
        source: str | Path, destination: str | Path, *, follow_symlinks: bool = True
    ) -> str | Path:
        nonlocal faulted
        if (
            scenario == "before-copy"
            and not faulted
            and Path(destination).name.startswith("before-")
        ):
            faulted = True
            message = "INERT_BEFORE_COPY"
            raise OSError(message)
        return copy_original(source, destination, follow_symlinks=follow_symlinks)

    def command(
        runner: public.Runner, args: Sequence[str], **options: Unpack[public.CommandOptions]
    ) -> bytes:
        original = list(args)
        input_path, output_path = options.get("input_path"), options.get("output_path")
        if args[0] == "/usr/bin/curl":
            args = [str(root / "bin/curl"), *args[1:]]
        calls.append(
            {
                "argv": normalized(cast("list[JsonValue]", original), root),
                "timeout": options.get("timeout", 30),
                "inputPath": normalized(str(input_path), root) if input_path else None,
                "outputPath": normalized(str(output_path), root) if output_path else None,
                "outputLimit": options.get("output_limit"),
                "stateAtStart": command_state(root, runner.attempt),
            }
        )
        nonlocal faulted
        try:
            result = run_original(runner, args, **options)
        finally:
            number = runner.number
            stdout = output_path or runner.attempt / f"{number:03d}.stdout"
            stderr = runner.attempt / f"{number:03d}.stderr"
            for label, path in (("stdout", stdout), ("stderr", stderr)):
                if path.exists():
                    data = path.read_bytes()
                    if len(data) > MAX_FILE:
                        message = "oracle response exceeded its closed fixture bound"
                        raise ValueError(message)
                    relative = f"responses/{number:03d}.{label}"
                    stage.write(root / relative, data)
                    obj(calls[-1])[label + "File"] = relative
                    obj(calls[-1])[label + "Pin"] = stage.pin(root / relative)
            obj(calls[-1])["status"] = children[-1][0].returncode if children else None
        state = obj(decode_json((root / "daemon.json").read_bytes()))
        last = obj(cast("list[JsonValue]", state["calls"])[-1])
        if (
            not faulted
            and last["role"] == "candidate-up"
            and scenario
            in {"candidate-up-close", "candidate-up-close-rollback-up", "candidate-up-late-result"}
        ):
            faulted = True
            if scenario == "candidate-up-late-result":
                raise TimeoutExpired(list(args), options.get("timeout", 30))
            message = "INERT_COMPLETED_CANDIDATE_CLOSE"
            raise OSError(message)
        return result

    def pump(
        child: subprocess.Popen[bytes],
        out: BinaryIO,
        err: BinaryIO,
        limits: bounded_process.Limits,
        *,
        input_data: bytes = b"",
    ) -> int:
        children.append((child, out, err))
        obj(calls[-1]).update({"stdoutLimit": limits.stdout, "stderrLimit": limits.stderr})
        return pump_original(child, out, err, limits, input_data=input_data)

    held: BinaryIO | None = None
    handlers = {number: signal.getsignal(number) for number in (signal.SIGINT, signal.SIGTERM)}
    previous_mask = os.umask(0o077)
    try:
        if scenario == "lock-busy":
            held = (root / "work/workload.lock").open("r+b")
            fcntl.flock(held.fileno(), fcntl.LOCK_EX | fcntl.LOCK_NB)
        with (
            ExitStack() as stack,
            redirect_stdout(output),
            redirect_stderr(error),
            patch.object(public, "ROOT", root / "public"),
            patch.object(public, "CONFIG", root / "config"),
            patch.object(public, "WORK", root / "work"),
            patch.object(public, "ROOT_UID", os.getuid()),
            patch.object(os, "geteuid", return_value=0),
            patch.object(
                public,
                "DOCKER",
                [str(root / "bin/docker"), "--host", "unix:///var/run/docker.sock"],
            ),
            patch.object(public, "JOURNAL_SELECTION", root / "journal.enabled"),
            patch.object(public, "timestamp", side_effect=timestamp),
            patch.object(
                public,
                "time",
                SimpleNamespace(monotonic=monotonic, sleep=sleep, time=lambda: 2524608000),
            ),
            patch.object(uuid, "uuid4", return_value=SimpleNamespace(hex=stage.UUID)),
            patch.object(
                shutil,
                "disk_usage",
                return_value=SimpleNamespace(free=cast("int", control["freeBytes"])),
            ),
            patch.object(public, "atomic", side_effect=atomic),
            patch.object(os, "fsync", side_effect=fsync),
            patch.object(Path, "replace", new=replace),
            patch.object(shutil, "copyfile", side_effect=copy),
            patch.object(public.Runner, "run", new=command),
            patch.object(bounded_process, "pump", side_effect=pump),
        ):
            _ = stack.enter_context(
                patch.object(
                    sys,
                    "argv",
                    [
                        "release-public.py",
                        "deploy",
                        REVISION,
                        "--quiet-seconds",
                        str(control["quietSeconds"]),
                    ],
                )
            )
            code = public.cli()
    finally:
        if held is not None:
            held.close()
        for number, handler in handlers.items():
            _ = signal.signal(number, handler)
        _ = os.umask(previous_mask)
    return {
        "exitCodes": [code],
        "reports": [normalized(decode_json(line), root) for line in output.getvalue().splitlines()],
        "stderr": error.getvalue(),
        "writes": writes,
        "atomicIntended": atomic_intended,
        "commands": calls,
        "children": [
            {
                "status": child.returncode,
                "pipesClosed": all(
                    stream is None or stream.closed
                    for stream in (child.stdin, child.stdout, child.stderr)
                ),
                "logsClosed": out.closed and err.closed,
            }
            for child, out, err in children
        ],
        "lockReleased": stage.lock_available(root / "work/workload.lock"),
        "faultObserved": faulted,
        "daemon": normalized(decode_json((root / "daemon.json").read_bytes()), root),
    }


def run(scenario: str, fixture_directory: Path | None = None) -> JsonObject:
    """Execute one closed whole reference and preserve independent pristine native inputs."""
    if scenario not in SCENARIOS:
        message = "unknown deploy oracle scenario"
        raise ValueError(message)
    if fixture_directory is None:
        with tempfile.TemporaryDirectory(prefix="deploy-oracle.") as name:
            return run(scenario, Path(name).resolve())
    root = fixture_directory
    if not root.is_absolute() or root.resolve() != root or any(root.iterdir()):
        message = "deploy fixture must be fresh and canonical"
        raise ValueError(message)
    root.chmod(0o700)
    sources = python_pins()
    originals = baseline_pins(sources)
    pins: JsonObject = {name: stage.pin(ROOT / name) for name in REFERENCE_PATHS}
    for label in ("native", "reference"):
        stage.directory(root / label)
        control = fixture(root / label, scenario)
    reference = root / "reference"
    inputs = inventory(reference / "public/releases", reference)
    input_identities = identities(reference / "public/releases")
    selection = inventory(reference / "config", reference)
    status, out, err = bounded_process.run(
        [sys.executable, "-B", str(Path(__file__).resolve()), "--reference-worker", str(reference)],
        limits=bounded_process.Limits(timeout=WORKER_SECONDS, stdout=MAX_OUTPUT, stderr=65536),
        env={"PATH": "/usr/bin:/bin", "PYTHONDONTWRITEBYTECODE": "1"},
    )
    if status or err:
        message = "complete original deployment worker failed"
        raise ValueError(message)
    observation = obj(decode_json(out))
    extra: JsonObject = {
        "attempts": [
            inventory(path, reference)
            for path in sorted((reference / "public/results").glob("release.*"))
        ],
        "releaseFiles": inventory(reference / "public/releases", reference),
        "configBefore": selection,
        "configAfter": inventory(reference / "config", reference),
        "assetFiles": inventory(reference / "public/browser-assets", reference),
        "inputPinsBefore": inputs,
        "inputIdentityBefore": input_identities,
        "inputIdentityAfter": {
            name: identities(reference / "public/releases").get(name) for name in input_identities
        },
        "inputPinsAfter": {
            name: value
            for name, value in inventory(reference / "public/releases", reference).items()
            if name in inputs
        },
        "releaseState": normalized(
            decode_json((reference / "public/release-state.json").read_bytes()), reference
        )
        if (reference / "public/release-state.json").exists()
        else None,
        "sourcePins": pins,
        "sourceUnchanged": pins == {name: stage.pin(ROOT / name) for name in REFERENCE_PATHS},
        "originalBaselinePythonPins": originals,
        "originalBaselinePythonUnchanged": originals == baseline_pins(python_pins()),
        "originalPythonUnchanged": sources == python_pins(),
        "originalPythonCount": len(sources),
        "workerSettled": {
            "status": status,
            "reaped": True,
            "pipesClosed": True,
            "stderrBytes": len(err),
        },
    }
    observation.update(extra)
    return {"schemaVersion": 1, "scenario": scenario, "input": control, "observation": observation}


# Exact maintained baseline names, generated from the source-only author manifest.
BASELINE_PYTHON_PATHS: tuple[str, ...] = (
    "build/bootstrap.py",
    "build/bootstrap_access.py",
    "build/bootstrap_controller.py",
    "build/build-release.py",
    "build/capacity.py",
    "build/capacity_artifacts.py",
    "build/ci-local-evidence.py",
    "build/ci_cache_retention.py",
    "build/ci_local_act.py",
    "build/ci_release_checks.py",
    "build/ci_verified.py",
    "build/deploy.py",
    "build/fetch-release.py",
    "build/force_deploy.py",
    "build/issue_account_recovery.py",
    "build/migrate.py",
    "build/migration_artifact.py",
    "build/migration_controller.py",
    "build/migration_dns.py",
    "build/native_security.py",
    "build/native_security_cache.py",
    "build/release_attestation.py",
    "build/release_build.py",
    "build/release_container_fixture.py",
    "build/release_container_harness.py",
    "build/release_deploy.py",
    "build/release_fetch_controller.py",
    "build/release_http_policy.py",
    "build/run-vps-capacity.py",
    "build/security_actions.py",
    "build/security_archive.py",
    "build/security_awslc.py",
    "build/security_check.py",
    "build/security_codeql.py",
    "build/security_codeql_cache.py",
    "build/security_codeql_cargo.py",
    "build/security_codeql_local.py",
    "build/security_codeql_resources.py",
    "build/security_codeql_rust_sources.py",
    "build/security_codeql_sources.py",
    "build/security_codeql_tools.py",
    "build/security_codeql_triage.py",
    "build/security_context.py",
    "build/security_dependency_licenses.py",
    "build/security_elf.py",
    "build/security_findings.py",
    "build/security_image.py",
    "build/security_image_advisories.py",
    "build/security_image_cache.py",
    "build/security_image_policy.py",
    "build/security_license_identity.py",
    "build/security_mutation.py",
    "build/security_native.py",
    "build/security_openssl.py",
    "build/security_policy.py",
    "build/security_rpm_notices.py",
    "build/security_rulesets.py",
    "build/security_runtime.py",
    "build/security_secret_projection.py",
    "build/security_secret_spans.py",
    "build/security_secrets.py",
    "build/security_semgrep.py",
    "build/security_semgrep_summary.py",
    "build/security_source_scope.py",
    "build/security_tools.py",
    "build/security_vendor.py",
    "build/security_vm.py",
    "build/security_vm_fetch.py",
    "build/security_vm_fetch_guest.py",
    "build/security_vm_fetch_hosts.py",
    "build/security_vm_guest.py",
    "build/security_vm_ops.py",
    "build/security_vm_ops_guest.py",
    "build/security_vm_qmp.py",
    "build/security_vm_receive.py",
    "build/security_vm_receive_guest.py",
    "build/security_vm_receive_route.py",
    "build/security_vm_release.py",
    "build/security_vm_stage.py",
    "build/security_vm_stage_guest.py",
    "build/security_vm_vps_guest.py",
    "build/test-release-container.py",
    "build/verify-release.py",
    "build/vps_capacity_controller.py",
    "build/vps_capacity_remote.py",
    "build/vps_ops_bootstrap.py",
    "ops/ansible/callback_plugins/release_timing.py",
    "ops/ansible/files/backup_offsite.py",
    "ops/ansible/files/backup_public.py",
    "ops/ansible/files/bounded_process.py",
    "ops/ansible/files/browser_assets.py",
    "ops/ansible/files/database_logging.py",
    "ops/ansible/files/fetch-release.py",
    "ops/ansible/files/force_release.py",
    "ops/ansible/files/migrate_public.py",
    "ops/ansible/files/migration_inspect.py",
    "ops/ansible/files/migration_snapshot.py",
    "ops/ansible/files/migration_tls.py",
    "ops/ansible/files/monitoring_alerts.py",
    "ops/ansible/files/monitoring_collect.py",
    "ops/ansible/files/monitoring_external.py",
    "ops/ansible/files/monitoring_prepare.py",
    "ops/ansible/files/monitoring_report.py",
    "ops/ansible/files/reboot-public.py",
    "ops/ansible/files/reboot_public.py",
    "ops/ansible/files/release-public.py",
    "ops/ansible/files/release_artifact.py",
    "ops/ansible/files/release_assets.py",
    "ops/ansible/files/release_fetch_receiver.py",
    "ops/ansible/files/release_json.py",
    "ops/ansible/files/release_preflight.py",
    "ops/ansible/files/release_public.py",
    "ops/ansible/files/release_trust.py",
    "ops/ansible/files/restore_verify.py",
    "ops/ansible/files/runtime_profile.py",
    "ops/ansible/files/seed-public-room.py",
    "ops/ansible/files/seed_public_room.py",
    "ops/ansible/files/turn_public.py",
    "ops/ansible/filter_plugins/sizing.py",
    "ops/ansible/tests/release_build_oracle.py",
    "ops/ansible/tests/release_fetch_controller_oracle.py",
    "ops/ansible/tests/release_fresh_verify_oracle.py",
    "ops/ansible/tests/release_prepare_oracle.py",
    "ops/ansible/tests/release_receive_oracle.py",
    "ops/ansible/tests/release_stage_oracle.py",
    "ops/ansible/tests/release_verify_oracle.py",
    "ops/ansible/tests/test_automation.py",
    "ops/ansible/tests/test_backup_offsite.py",
    "ops/ansible/tests/test_bootstrap_controller.py",
    "ops/ansible/tests/test_bounded_process.py",
    "ops/ansible/tests/test_browser_assets_command.py",
    "ops/ansible/tests/test_capacity.py",
    "ops/ansible/tests/test_capacity_artifacts.py",
    "ops/ansible/tests/test_capacity_container.py",
    "ops/ansible/tests/test_capacity_ownership.py",
    "ops/ansible/tests/test_ci_cache_retention.py",
    "ops/ansible/tests/test_ci_database_check.py",
    "ops/ansible/tests/test_ci_local_act.py",
    "ops/ansible/tests/test_ci_local_evidence.py",
    "ops/ansible/tests/test_ci_release_checks.py",
    "ops/ansible/tests/test_ci_verified.py",
    "ops/ansible/tests/test_database_logging.py",
    "ops/ansible/tests/test_deploy_controller.py",
    "ops/ansible/tests/test_dns_handoff.py",
    "ops/ansible/tests/test_force_release.py",
    "ops/ansible/tests/test_host_playbooks.py",
    "ops/ansible/tests/test_issue_account_recovery.py",
    "ops/ansible/tests/test_migrate_public.py",
    "ops/ansible/tests/test_migration_abort.py",
    "ops/ansible/tests/test_migration_artifact.py",
    "ops/ansible/tests/test_migration_controller.py",
    "ops/ansible/tests/test_migration_dns.py",
    "ops/ansible/tests/test_migration_inspect.py",
    "ops/ansible/tests/test_migration_prepare.py",
    "ops/ansible/tests/test_migration_tls.py",
    "ops/ansible/tests/test_monitoring.py",
    "ops/ansible/tests/test_monitoring_database.py",
    "ops/ansible/tests/test_monitoring_external.py",
    "ops/ansible/tests/test_native_security.py",
    "ops/ansible/tests/test_ops_artifact_workflow.py",
    "ops/ansible/tests/test_ops_runtime_workflow.py",
    "ops/ansible/tests/test_performance_compare_ci.py",
    "ops/ansible/tests/test_public_backup.py",
    "ops/ansible/tests/test_public_deployment.py",
    "ops/ansible/tests/test_public_reboot.py",
    "ops/ansible/tests/test_public_release.py",
    "ops/ansible/tests/test_public_seed.py",
    "ops/ansible/tests/test_public_templates.py",
    "ops/ansible/tests/test_python_quality.py",
    "ops/ansible/tests/test_release_assets.py",
    "ops/ansible/tests/test_release_attestation.py",
    "ops/ansible/tests/test_release_build.py",
    "ops/ansible/tests/test_release_build_oracle.py",
    "ops/ansible/tests/test_release_container_ci.py",
    "ops/ansible/tests/test_release_container_fixture.py",
    "ops/ansible/tests/test_release_container_harness.py",
    "ops/ansible/tests/test_release_fetch.py",
    "ops/ansible/tests/test_release_fetch_controller.py",
    "ops/ansible/tests/test_release_fetch_controller_oracle.py",
    "ops/ansible/tests/test_release_fetch_runtime_workflow.py",
    "ops/ansible/tests/test_release_fresh_verify_oracle.py",
    "ops/ansible/tests/test_release_http_policy.py",
    "ops/ansible/tests/test_release_json.py",
    "ops/ansible/tests/test_release_playbook.py",
    "ops/ansible/tests/test_release_preflight.py",
    "ops/ansible/tests/test_release_prepare_oracle.py",
    "ops/ansible/tests/test_release_receive_oracle.py",
    "ops/ansible/tests/test_release_stage_oracle.py",
    "ops/ansible/tests/test_release_timing.py",
    "ops/ansible/tests/test_release_verify_oracle.py",
    "ops/ansible/tests/test_release_workflow.py",
    "ops/ansible/tests/test_restore_database.py",
    "ops/ansible/tests/test_restore_verify.py",
    "ops/ansible/tests/test_runtime_components.py",
    "ops/ansible/tests/test_runtime_profile.py",
    "ops/ansible/tests/test_security_actions.py",
    "ops/ansible/tests/test_security_archive.py",
    "ops/ansible/tests/test_security_base_secrets.py",
    "ops/ansible/tests/test_security_check.py",
    "ops/ansible/tests/test_security_ci.py",
    "ops/ansible/tests/test_security_codeql_cache.py",
    "ops/ansible/tests/test_security_codeql_cargo.py",
    "ops/ansible/tests/test_security_codeql_health.py",
    "ops/ansible/tests/test_security_codeql_local.py",
    "ops/ansible/tests/test_security_codeql_resources.py",
    "ops/ansible/tests/test_security_codeql_rust_sources.py",
    "ops/ansible/tests/test_security_codeql_sources.py",
    "ops/ansible/tests/test_security_codeql_triage.py",
    "ops/ansible/tests/test_security_context.py",
    "ops/ansible/tests/test_security_dependency_licenses.py",
    "ops/ansible/tests/test_security_elf.py",
    "ops/ansible/tests/test_security_fedora_license.py",
    "ops/ansible/tests/test_security_findings.py",
    "ops/ansible/tests/test_security_image_advisories.py",
    "ops/ansible/tests/test_security_image_cache.py",
    "ops/ansible/tests/test_security_image_evidence.py",
    "ops/ansible/tests/test_security_image_finalization.py",
    "ops/ansible/tests/test_security_image_parallel.py",
    "ops/ansible/tests/test_security_image_policy.py",
    "ops/ansible/tests/test_security_image_prepare.py",
    "ops/ansible/tests/test_security_image_runner.py",
    "ops/ansible/tests/test_security_image_secrets.py",
    "ops/ansible/tests/test_security_license_identity.py",
    "ops/ansible/tests/test_security_migrations.py",
    "ops/ansible/tests/test_security_mutation.py",
    "ops/ansible/tests/test_security_native.py",
    "ops/ansible/tests/test_security_openssl.py",
    "ops/ansible/tests/test_security_policy.py",
    "ops/ansible/tests/test_security_rpm_license.py",
    "ops/ansible/tests/test_security_rpm_notices.py",
    "ops/ansible/tests/test_security_rpm_secrets.py",
    "ops/ansible/tests/test_security_rulesets.py",
    "ops/ansible/tests/test_security_runtime.py",
    "ops/ansible/tests/test_security_runtime_licenses.py",
    "ops/ansible/tests/test_security_secret_projection.py",
    "ops/ansible/tests/test_security_secret_spans.py",
    "ops/ansible/tests/test_security_secrets.py",
    "ops/ansible/tests/test_security_semgrep.py",
    "ops/ansible/tests/test_security_semgrep_summary.py",
    "ops/ansible/tests/test_security_tools.py",
    "ops/ansible/tests/test_security_vendor.py",
    "ops/ansible/tests/test_security_vm.py",
    "ops/ansible/tests/test_security_vm_boot.py",
    "ops/ansible/tests/test_security_vm_fetch.py",
    "ops/ansible/tests/test_security_vm_fetch_guest.py",
    "ops/ansible/tests/test_security_vm_fetch_hosts.py",
    "ops/ansible/tests/test_security_vm_guest.py",
    "ops/ansible/tests/test_security_vm_ops.py",
    "ops/ansible/tests/test_security_vm_ops_guest.py",
    "ops/ansible/tests/test_security_vm_qmp.py",
    "ops/ansible/tests/test_security_vm_receive.py",
    "ops/ansible/tests/test_security_vm_release.py",
    "ops/ansible/tests/test_security_vm_stage.py",
    "ops/ansible/tests/test_security_vm_vps_guest.py",
    "ops/ansible/tests/test_session_soak_ci.py",
    "ops/ansible/tests/test_support.py",
    "ops/ansible/tests/test_turn_public.py",
    "ops/ansible/tests/test_vps_capacity_controller.py",
    "ops/ansible/tests/test_vps_capacity_oracle.py",
    "ops/ansible/tests/test_vps_capacity_remote.py",
    "ops/ansible/tests/test_vps_ops_bootstrap.py",
    "ops/ansible/tests/vps_capacity_oracle.py",
    "vendor/mediasoup-sys-0.19.0/subprojects/packagefiles/libuv/link_file_in_build_dir.py",
    "vendor/mediasoup-sys-0.19.0/tasks.py",
    "xtask/src/ci/release_http_policy/__init__.py",
    "xtask/src/ci/release_http_policy/oracle.py",
    "xtask/src/ci/release_http_policy/reference/build/release_container_fixture.py",
    "xtask/src/ci/release_http_policy/reference/build/release_container_harness.py",
    "xtask/src/ci/release_http_policy/reference/build/release_http_policy.py",
    "xtask/src/ci/release_http_policy/reference/ops/ansible/files/bounded_process.py",
    "xtask/src/ci/release_http_policy/reference/ops/ansible/files/release_artifact.py",
    "xtask/src/ci/release_http_policy/reference/ops/ansible/files/release_assets.py",
    "xtask/src/ci/release_http_policy/reference/ops/ansible/files/release_json.py",
    "xtask/src/ci/release_http_policy/reference/ops/ansible/files/release_public.py",
    "xtask/src/ci/release_http_policy/reference/ops/ansible/files/runtime_profile.py",
    (
        "xtask/src/ci/release_http_policy/reference/ops/ansible/tests/"
        + "test_release_container_harness.py"
    ),
    "xtask/src/ci/release_http_policy/reference/ops/ansible/tests/test_release_http_policy.py",
    "xtask/src/ci/release_http_policy/reference/ops/ansible/tests/test_support.py",
)


class Arguments(argparse.Namespace):
    """Expose only closed scenarios or this private owned worker role."""

    scenario: str | None = None
    fixture_directory: Path | None = None
    reference_worker: Path | None = None


def main(argv: list[str] | None = None) -> int:
    """Emit one bounded ordinary JSON object after complete owned reference settlement."""
    parser = argparse.ArgumentParser(description=__doc__)
    _ = parser.add_argument("--scenario", choices=SCENARIOS)
    _ = parser.add_argument("--fixture-directory", type=Path)
    _ = parser.add_argument("--reference-worker", type=Path, help=argparse.SUPPRESS)
    args = parser.parse_args(argv, namespace=Arguments())
    if args.reference_worker is not None:
        result = worker(args.reference_worker)
    elif args.scenario is not None:
        result = run(args.scenario, args.fixture_directory)
    else:
        parser.error("one closed scenario required")
    data = encoded(result)
    if len(data) > MAX_OUTPUT:
        message = "deploy oracle output too large"
        raise ValueError(message)
    if sys.stdout.buffer.write(data) != len(data):
        message = "deploy oracle incomplete stdout"
        raise OSError(message)
    _ = sys.stdout.buffer.flush()
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
