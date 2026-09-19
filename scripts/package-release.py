"""Package an already-built Linux image using explicit public-file allowlists."""
from pathlib import Path
import gzip, hashlib, json, shutil, subprocess, tarfile, zipfile

ROOT = Path(__file__).resolve().parent.parent
VERSION = '20260907'
IMAGE = f'xlh:{VERSION}'
OUT = ROOT / 'output' / f'release-{VERSION}'
OUT.mkdir(parents=True, exist_ok=True)
STAGE = OUT / f'xlh-linux-x86_64-release-{VERSION}'
STAGE.mkdir(exist_ok=True)

def run(*args):
    return subprocess.check_output(args, cwd=ROOT, text=True).strip()

def checksum(path):
    with path.open('rb') as stream:
        digest = hashlib.file_digest(stream, 'sha256').hexdigest()
    path.with_name(path.name + '.sha256').write_text(f'{digest}  {path.name}\n', encoding='ascii')
    return digest

image_info = json.loads(run('docker', 'image', 'inspect', IMAGE))[0]
assert image_info['Os'] == 'linux' and image_info['Architecture'] == 'amd64'
container = run('docker', 'create', IMAGE)
try:
    subprocess.run(['docker','cp',f'{container}:/usr/local/bin/xlh',str(STAGE/'xlh')], check=True)
finally:
    subprocess.run(['docker','rm',container], check=True, stdout=subprocess.DEVNULL)
binary = STAGE / 'xlh'
assert binary.read_bytes()[:4] == b'\x7fELF'
binary.chmod(0o755)

files = {
    'config.toml':'config.example.toml', '.env.example':'.env.example',
    'docker-compose.prod.yml':'docker-compose.prod.yml',
    'docs/release-20260907.md':'README.md',
    'docs/forecast-tracking.md':'forecast-tracking.md',
    'scripts/backup-all-data.sh':'backup-all-data.sh',
}
for src, dest in files.items():
    text = (ROOT/src).read_text(encoding='utf-8')
    if src == '.env.example': text = text.replace('XLH_IMAGE=xlh:latest', f'XLH_IMAGE={IMAGE}')
    (STAGE/dest).write_text(text, encoding='utf-8', newline='\n')

source_files = [ROOT/'Cargo.toml',ROOT/'Cargo.lock',ROOT/'Dockerfile'] + sorted((ROOT/'src').rglob('*.rs'))
metadata = {
    'version':VERSION,'image':IMAGE,'image_id':image_info['Id'],
    'platform':'linux/amd64','base_commit':run('git','rev-parse','HEAD'),
    'includes_uncommitted_changes':bool(run('git','status','--porcelain')),
    'binary_sha256':checksum(binary),
    'source_sha256':{str(p.relative_to(ROOT)).replace('\\','/'):hashlib.sha256(p.read_bytes()).hexdigest() for p in source_files},
}
(STAGE/'BUILD.json').write_text(json.dumps(metadata,indent=2),encoding='utf-8')
archive=OUT/(STAGE.name+'.tar.gz')
with tarfile.open(archive,'w:gz') as tar:
    for p in sorted(STAGE.iterdir()):
        info=tar.gettarinfo(str(p),arcname=f'{STAGE.name}/{p.name}')
        info.mode=0o755 if p.name in ('xlh','backup-all-data.sh') else 0o644
        info.uid=info.gid=0;info.uname=info.gname=''
        with p.open('rb') as stream:tar.addfile(info,stream)
checksum(archive)

image_tar=OUT/f'xlh-image-linux-amd64-{VERSION}.tar'
if image_tar.exists(): raise FileExistsError(image_tar)
subprocess.run(['docker','save','-o',str(image_tar),IMAGE],check=True)
image_gz=image_tar.with_suffix('.tar.gz')
with image_tar.open('rb') as src,gzip.open(image_gz,'wb',compresslevel=6) as dst:shutil.copyfileobj(src,dst)
checksum(image_gz)
# Retain the intermediate image export; no cleanup of existing artifacts or data.
deploy=OUT/f'xlh-deploy-{VERSION}.zip'
with zipfile.ZipFile(deploy,'w',compression=zipfile.ZIP_DEFLATED) as z:
    for p in sorted(STAGE.iterdir()):
        if p.name not in ('xlh','xlh.sha256'):z.write(p,p.name)
checksum(deploy)
for p in [archive,image_gz,deploy]:print(f'{p}\n  {p.stat().st_size:,} bytes')
