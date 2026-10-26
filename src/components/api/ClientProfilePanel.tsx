/**
 * 全局 API 管理 · 实机环境（真机档案导入）
 *
 * 网关常部署在装不了 Trae 客户端的机器（Debian 容器），只能靠内置兜底值伪造身份。
 * 本页把真机导出的 profile JSON（scripts/export-trae-profile.ps1 / .py 生成）
 * 导入网关：版本身份、设备指纹、device_id/machine_id 全部以真机为准，并要求
 * 后端用真机 icube 凭证解出的私钥做 DeviceInfo/DeviceProof，做到链路自洽。
 *
 * 导入即生效（后端热应用），清除即回退内置/本机扫描值；私钥只存后端 kv，
 * 不回传前端（页面只显示「已导入私钥」）。
 */
import { useCallback, useEffect, useRef, useState } from 'react';
import { Fingerprint, RotateCcw, ShieldCheck, Upload } from 'lucide-react';
import { Badge, Spinner } from '../ui';
import { api } from '../../lib/tauri';
import { useAppStore } from '../../store';
import type { ClientProfileView } from '../../types';

/** 生效字段展示顺序（与真实客户端 commonParams 口径一致） */
const ROWS: { key: keyof ClientProfileView['profile']; label: string }[] = [
  { key: 'app_version', label: '客户端版本' },
  { key: 'build_version', label: '构建号' },
  { key: 'version_code', label: '版本号 code' },
  { key: 'package_type', label: '包类型' },
  { key: 'app_id', label: 'appId' },
  { key: 'os_name', label: '系统名' },
  { key: 'os_version', label: '系统版本' },
  { key: 'device_model', label: '设备型号' },
  { key: 'device_manufacturer', label: '设备厂商' },
  { key: 'cpu', label: 'CPU 厂商' },
  { key: 'device_id', label: 'device_id' },
  { key: 'machine_id', label: 'machine_id' },
];

export default function ClientProfilePanel() {
  const toast = useAppStore((s) => s.pushToast);
  const [view, setView] = useState<ClientProfileView | null>(null);
  const [content, setContent] = useState('');
  const [loading, setLoading] = useState(true);
  const [busy, setBusy] = useState(false);
  const [confirmClear, setConfirmClear] = useState(false);
  const clearTimer = useRef<ReturnType<typeof setTimeout> | null>(null);
  const fileRef = useRef<HTMLInputElement | null>(null);

  useEffect(
    () => () => {
      if (clearTimer.current) clearTimeout(clearTimer.current);
    },
    [],
  );

  const load = useCallback(async () => {
    setLoading(true);
    try {
      setView(await api.clientProfile.get());
    } catch (e) {
      toast('error', `读取实机环境失败：${e}`);
    } finally {
      setLoading(false);
    }
  }, [toast]);

  useEffect(() => {
    void load();
  }, [load]);

  const doImport = async () => {
    const text = content.trim();
    if (!text) {
      toast('error', '请先粘贴或选择真机导出的 profile JSON');
      return;
    }
    setBusy(true);
    try {
      const v = await api.clientProfile.import(text);
      setView(v);
      setContent('');
      toast('success', '实机环境档案已导入并立即生效');
    } catch (e) {
      toast('error', `导入失败：${e}`);
    } finally {
      setBusy(false);
    }
  };

  const doClear = async () => {
    setBusy(true);
    try {
      setView(await api.clientProfile.clear());
      toast('success', '已清除实机档案，回退内置身份');
    } catch (e) {
      toast('error', `清除失败：${e}`);
    } finally {
      setBusy(false);
    }
  };

  const onPickFile = async (file: File | undefined) => {
    if (!file) return;
    try {
      setContent(await file.text());
    } catch (e) {
      toast('error', `读取文件失败：${e}`);
    }
  };

  if (loading) {
    return (
      <div className="flex items-center justify-center py-16">
        <Spinner className="h-5 w-5" />
      </div>
    );
  }

  const imported = view?.imported ?? false;

  return (
    <div className="space-y-4">
      <div className="flex items-start gap-2 rounded-md border border-slate-200 p-3 text-xs text-slate-500 dark:border-zinc-700 dark:text-zinc-400">
        <Fingerprint size={15} className="mt-0.5 shrink-0" />
        <div>
          <p className="text-sm font-medium text-slate-700 dark:text-zinc-200">
            实机环境档案（全局唯一设备）
          </p>
          <p className="mt-1 opacity-80">
            在装有 Trae CN 的真机上运行导出脚本，把生成的 JSON 导入本网关。导入后版本身份、
            设备指纹、device_id/machine_id 全部以真机为准，并用真机 icube 凭证的私钥构造
            DeviceInfo/DeviceProof，登录、推理、模型同步三路身份完全一致。
          </p>
          <p className="mt-1 opacity-80">
            已有 device_map 绑定的账号不会被改写（改了会触发上游 Token device 不匹配）；
            新登录与无绑定账号统一使用该设备。
          </p>
        </div>
      </div>

      {/* 当前生效档案 */}
      <div className="flex items-center gap-2">
        <span className="text-sm font-medium">当前生效</span>
        <Badge tone={imported ? 'blue' : 'slate'}>{imported ? '实机导入' : '内置兜底'}</Badge>
        {imported && (
          <Badge tone={view?.has_private_key ? 'green' : 'amber'}>
            {view?.has_private_key ? '含真机私钥' : '无私钥'}
          </Badge>
        )}
        <div className="ml-auto flex items-center gap-2">
          <button
            onClick={() => {
              if (clearTimer.current) clearTimeout(clearTimer.current);
              if (confirmClear) {
                clearTimer.current = null;
                setConfirmClear(false);
                void doClear();
              } else {
                setConfirmClear(true);
                clearTimer.current = setTimeout(() => {
                  setConfirmClear(false);
                  clearTimer.current = null;
                }, 3000);
              }
            }}
            disabled={busy || !imported}
            className="flex items-center gap-1 rounded-md border border-slate-200 px-2.5 py-1.5 text-xs text-slate-600 transition hover:bg-slate-50 disabled:opacity-40 dark:border-zinc-700 dark:text-zinc-300 dark:hover:bg-zinc-800"
          >
            <RotateCcw size={13} />
            {confirmClear ? '确认清除？' : '清除档案'}
          </button>
        </div>
      </div>

      <div className="overflow-hidden rounded-md border border-slate-200 dark:border-zinc-700">
        <table className="w-full text-xs">
          <tbody>
            {ROWS.map((r) => {
              const value = view?.profile[r.key] ?? '';
              return (
                <tr
                  key={r.key}
                  className="border-b border-slate-100 last:border-b-0 dark:border-zinc-800"
                >
                  <td className="w-40 px-3 py-1.5 text-slate-500 dark:text-zinc-400">{r.label}</td>
                  <td className="px-3 py-1.5 font-mono break-all text-slate-700 dark:text-zinc-200">
                    {value || <span className="text-slate-300 dark:text-zinc-600">—</span>}
                  </td>
                </tr>
              );
            })}
          </tbody>
        </table>
      </div>

      {/* 导入区 */}
      <div className="space-y-2">
        <div className="flex items-center gap-2">
          <span className="text-sm font-medium">导入真机档案</span>
          <div className="ml-auto flex items-center gap-2">
            <input
              ref={fileRef}
              type="file"
              accept=".json,application/json"
              className="hidden"
              onChange={(e) => void onPickFile(e.target.files?.[0])}
            />
            <button
              onClick={() => fileRef.current?.click()}
              disabled={busy}
              className="flex items-center gap-1 rounded-md border border-slate-200 px-2.5 py-1.5 text-xs text-slate-600 transition hover:bg-slate-50 disabled:opacity-40 dark:border-zinc-700 dark:text-zinc-300 dark:hover:bg-zinc-800"
            >
              <Upload size={13} />
              选择文件
            </button>
            <button
              onClick={() => void doImport()}
              disabled={busy || !content.trim()}
              className="flex items-center gap-1 rounded-md bg-brand-500 px-3 py-1.5 text-xs font-medium text-white transition hover:bg-brand-600 disabled:opacity-40"
            >
              {busy ? <Spinner className="h-3.5 w-3.5" /> : <ShieldCheck size={13} />}
              导入并生效
            </button>
          </div>
        </div>
        <textarea
          value={content}
          onChange={(e) => setContent(e.target.value)}
          placeholder="粘贴真机导出的 profile JSON（含 identity / device / credential 三段）"
          spellCheck={false}
          className="h-40 w-full resize-y rounded-md border border-slate-200 bg-transparent p-2 font-mono text-xs outline-none focus:border-brand-500 dark:border-zinc-700"
        />
        <p className="text-xs text-slate-400">
          文件内 credential 段为 icube 密文，后端解密后仅存本地 kv，不回传前端、不写日志。
        </p>
      </div>
    </div>
  );
}
