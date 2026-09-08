from pathlib import Path
import shutil
import subprocess


def main() -> int:
    repository_root = Path(__file__).resolve().parent.parent
    node_executable = shutil.which('node')
    if node_executable is None:
        raise SystemExit('Node.js is required. Run npm run icons:generate after installing project dependencies.')
    return subprocess.run(
        [node_executable, str(repository_root / 'scripts' / 'generate_icons.mjs')],
        cwd=repository_root,
        check=False,
    ).returncode


if __name__ == '__main__':
    raise SystemExit(main())
