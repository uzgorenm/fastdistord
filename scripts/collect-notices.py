#!/usr/bin/env python3
"""Copy dependency license/notice text into distributable packages.

Cargo metadata is the inventory; this is not a license-compatibility classifier.
Build and test dependencies may be included conservatively.
"""
import json
import shutil
import subprocess
from pathlib import Path

out = Path('dist/third-party')
out.mkdir(parents=True, exist_ok=True)
(out / 'NOTICE_COLLECTION_COMPLETE').unlink(missing_ok=True)
host = next(line.split(': ', 1)[1] for line in subprocess.check_output(['rustc', '-vV'], text=True).splitlines() if line.startswith('host: '))
metadata = json.loads(subprocess.check_output(['cargo', 'metadata', '--locked', '--format-version', '1', '--filter-platform', host]))
lines = ['Dependency source notices (may include build/test dependencies).', '']
for package in sorted(metadata['packages'], key=lambda p: (p['name'], p['version'])):
    if package['name'] == 'fastdistord':
        continue
    root = Path(package['manifest_path']).parent
    name = f"{package['name']}-{package['version']}"
    lines.append(f"{name}: {package.get('license') or 'see source notices'}; {package.get('repository') or package.get('source') or 'vendored source'}")
    found = []
    for file in root.rglob('*'):
        upper = file.name.upper()
        if file.is_file() and (upper.startswith(('LICENSE', 'COPYING', 'NOTICE', 'COPYRIGHT', 'OFL')) or '-LICENSE' in upper):
            if '.git' not in file.parts and file.stat().st_size < 1024 * 1024:
                found.append((file, file.relative_to(root)))
    # Git workspace crates often share their repository-level license.
    for ancestor in list(root.parents)[:3]:
        if (ancestor / 'Cargo.toml').exists() or (ancestor / '.git').exists():
            for file in ancestor.iterdir():
                if file.is_file() and file.name.upper().startswith(('LICENSE', 'COPYING', 'NOTICE', 'COPYRIGHT')):
                    found.append((file, Path('workspace') / file.name))
    for file, relative in found:
        target = out / name / relative
        target.parent.mkdir(parents=True, exist_ok=True)
        shutil.copyfile(file, target)
(out / 'DEPENDENCIES.txt').write_text('\n'.join(lines) + '\n', encoding='utf-8')
# Keep the local patch attribution alongside Songbird's ISC license.
shutil.copyfile('vendor/songbird/FASTDISTORD_PATCH.md', out / 'SONGBIRD_PATCH.md')
if not any(out.glob('fastframe-fonts-*/fonts/Inter-LICENSE.txt')):
    raise SystemExit('Missing embedded Inter font license')
if not any(out.glob('songbird-*/LICENSE.md')):
    raise SystemExit('Missing Songbird license')
sysroot = Path(subprocess.check_output(['rustc', '--print', 'sysroot'], text=True).strip())
rust_doc = sysroot / 'share/doc/rust'
if (rust_doc / 'COPYRIGHT-library.html').is_file():
    shutil.copytree(rust_doc / 'licenses', out / 'rust-standard-library/licenses', dirs_exist_ok=True)
    shutil.copyfile(rust_doc / 'COPYRIGHT-library.html', out / 'rust-standard-library/COPYRIGHT-library.html')
(out / 'NOTICE_COLLECTION_COMPLETE').write_text('Source notices collected successfully.\n', encoding='utf-8')
print(f'Collected {len(metadata["packages"])} dependency inventories and source notices.')
