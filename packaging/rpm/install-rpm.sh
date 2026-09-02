#!/usr/bin/env bash
set -euo pipefail

if (( $# != 1 )); then
    echo "Usage: $0 /path/to/lyrune.rpm" >&2
    exit 2
fi
if (( EUID == 0 )); then
    echo "请以普通用户运行此脚本；安装步骤会调用 sudo。" >&2
    exit 1
fi
package="$(realpath -- "$1")"
[[ "$(rpm -qp --queryformat '%{NAME}' "$package")" == lyrune ]] || {
    echo "请选择 Lyrune RPM 安装包。" >&2
    exit 1
}
sudo dnf install "$package"

# Earlier source builds may have a per-user launcher overriding the installed
# /usr/share/applications/lyrune.desktop. Back up only this checkout's launcher;
# never edit user files from an RPM script running as root.
script_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
repo_dir="$(cd -- "$script_dir/../.." && pwd)"
data_dir="${XDG_DATA_HOME:-$HOME/.local/share}"
python3 - "$data_dir/applications/lyrune.desktop" "$repo_dir/target/release/lyrune" <<'PY'
import configparser
from pathlib import Path
import sys
import time

path = Path(sys.argv[1])
if path.exists():
    config = configparser.ConfigParser(interpolation=None, strict=False)
    config.read(path, encoding='utf-8')
    command = config.get('Desktop Entry', 'Exec', fallback='')
    if command in (sys.argv[2], '"' + sys.argv[2] + '"'):
        backup = path.with_name(path.name + '.before-rpm-' + str(time.time_ns()))
        path.rename(backup)
        print('已备份旧源码启动项：', backup)
    else:
        print('检测到自定义用户启动项，已保留：', path)
        print('如启动器仍打开旧版，请检查该启动项的 Exec 和 Icon。')
PY
if command -v kbuildsycoca6 >/dev/null; then
    kbuildsycoca6 --noincremental
fi
echo '安装完成。在应用程序启动器中搜索 Lyrune 即可打开。'
echo '如果旧版仍在运行，请先从托盘退出，再启动安装版。'
