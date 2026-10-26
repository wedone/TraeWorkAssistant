#!/usr/bin/env python3
"""导出本机 Trae CN 客户端环境档案（实机环境），供网关「实机环境」页签导入。

与 scripts/export-trae-profile.ps1 输出同构：
  identity   版本真身（manifest.json / product.json）
  device     telemetry.machineId、device_id、系统型号/厂商/CPU/系统版本
  credential storage.json 中 iCubeAuthInfo://icube-dc:<deviceId> 的 tc 信封密文

私钥明文不写入导出文件，由后端导入时解密。
仅用标准库，Windows 上无需安装任何依赖。
"""

from __future__ import annotations

import argparse
import json
import os
import sys
from datetime import datetime, timezone
from pathlib import Path

try:
    import winreg
except ImportError:  # 非 Windows
    winreg = None


def read_json(path: Path) -> dict | None:
    try:
        return json.loads(path.read_text(encoding="utf-8-sig"))
    except (OSError, ValueError):
        return None


def find_install_dir(app_dir: str | None) -> Path:
    if app_dir:
        return Path(app_dir)
    local = os.environ.get("LOCALAPPDATA", "")
    for name in ("Trae CN", "TRAE SOLO CN"):
        cand = Path(local) / "Programs" / name
        if (cand / "manifest.json").exists():
            return cand
    raise SystemExit(
        "未找到 Trae 客户端安装目录（Trae CN / TRAE SOLO CN），请用 --app-dir 指定"
    )


def reg_value(root, subkey: str, name: str) -> str:
    if winreg is None:
        return ""
    try:
        with winreg.OpenKey(root, subkey) as k:
            return str(winreg.QueryValueEx(k, name)[0])
    except OSError:
        return ""


def cpu_vendor(cpu_name: str) -> str:
    low = cpu_name.lower()
    if "intel" in low:
        return "Intel"
    if "amd" in low:
        return "AMD"
    return cpu_name


def main() -> int:
    ap = argparse.ArgumentParser(description="导出 Trae CN 实机环境档案")
    ap.add_argument("-o", "--out-file", default="trae-client-profile.json")
    ap.add_argument("--app-dir", default=None)
    args = ap.parse_args()

    install = find_install_dir(args.app_dir)
    manifest = read_json(install / "manifest.json")
    if not manifest:
        raise SystemExit(f"manifest.json 读取失败: {install}")
    product = read_json(install / "resources" / "app" / "product.json") or {}

    version_code = (manifest.get("ahaNet", {}).get("ttnet_params", {}) or {}).get(
        "version_code", ""
    ) or product.get("traeVersionCode", "")

    storage_path = (
        Path(os.environ.get("APPDATA", ""))
        / install.name
        / "User"
        / "globalStorage"
        / "storage.json"
    )
    storage = read_json(storage_path)
    if storage is None:
        raise SystemExit(
            f"storage.json 读取失败: {storage_path}（客户端至少启动登录过一次才会生成）"
        )

    machine_id = storage.get("telemetry.machineId", "") or ""
    dev_device_id = storage.get("telemetry.devDeviceId", "") or ""

    storage_key = ""
    tc_envelope = ""
    device_id = ""
    for key, val in storage.items():
        if key.startswith("iCubeAuthInfo://icube-dc:"):
            storage_key = key
            tc_envelope = val if isinstance(val, str) else ""
            device_id = key.rsplit(":", 1)[-1]
            break
    if not tc_envelope:
        print(
            "警告: 未找到 icube 设备凭证，导入后将无私钥，DeviceInfo/DeviceProof 无法与真机同源",
            file=sys.stderr,
        )

    bios = r"HARDWARE\DESCRIPTION\System\BIOS"
    cpu_key = r"HARDWARE\DESCRIPTION\System\CentralProcessor\0"
    win_key = r"SOFTWARE\Microsoft\Windows NT\CurrentVersion"
    device_model = reg_value(winreg.HKEY_LOCAL_MACHINE, bios, "SystemProductName") if winreg else ""
    device_manufacturer = (
        reg_value(winreg.HKEY_LOCAL_MACHINE, bios, "SystemManufacturer") if winreg else ""
    )
    cpu_name = reg_value(winreg.HKEY_LOCAL_MACHINE, cpu_key, "ProcessorNameString") if winreg else ""
    os_version = reg_value(winreg.HKEY_LOCAL_MACHINE, win_key, "ProductName") if winreg else ""

    out = {
        "schema": 1,
        "exported_at": datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ"),
        "source_app": install.name,
        "identity": {
            "app_version": manifest.get("appVersion", ""),
            "build_version": manifest.get("buildVersion", ""),
            "version_code": str(version_code),
            "package_type": manifest.get("packageType", ""),
            "app_id": manifest.get("appId", ""),
        },
        "device": {
            "device_id": device_id,
            "machine_id": machine_id,
            "dev_device_id": dev_device_id,
            "device_model": device_model,
            "device_manufacturer": device_manufacturer,
            "cpu": cpu_vendor(cpu_name),
            "os_name": "windows",
            "os_version": os_version,
        },
        "credential": {"storage_key": storage_key, "tc_envelope": tc_envelope},
    }

    Path(args.out_file).write_text(
        json.dumps(out, ensure_ascii=False, indent=2), encoding="utf-8"
    )
    print(f"已导出: {args.out_file}")
    print(
        f"  客户端: {out['identity']['app_version']} ({out['identity']['build_version']})"
        f" / code {out['identity']['version_code']}"
    )
    print(
        f"  设备: device_id={device_id or '-'} machine_id={'已读取' if machine_id else '-'}"
    )
    print(f"  凭证: {'已包含（密文）' if tc_envelope else '缺失'}")
    print("注意: 文件含设备凭证密文，请勿外传；导入网关后建议删除本地副本。")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
