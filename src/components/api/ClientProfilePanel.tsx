/**
 * 全局 API 管理 · 实机环境（设备池 + 账号绑定）
 *
 * 网关常部署在装不了 Trae 客户端的机器（Debian 容器），只能靠内置兜底值伪造身份。
 * 在装有 Trae CN 的真机上运行导出脚本（scripts/export-trae-profile.ps1 / .py）拿到
 * profile JSON，导入本页形成设备池；再把具体账号绑定到某台真机。
 *
 * 作用域是**账号级**：未绑定账号一律走派生/合成身份，绝不与已绑定账号共用同一台真机
 * （多账号共用一台设备恰好是上游风控识别的典型特征）。私钥只存后端 kv，不回传前端。
 */
import { useCallback, useEffect, useRef, useState } from 'react';
import { Fingerprint, Link2, RotateCcw, ShieldCheck, Trash2, Unlink, Upload } from 'lucide-react';
import { Badge, Spinner } from '../ui';
import { api } from '../../lib/tauri';
import { useAppStore } from '../../store';
import type { AccountView, ClientProfilePoolView } from '../../types';

/** 未绑定账号实际使用的内置身份（与真实客户端 commonParams 口径一致） */
const BUILTIN_ROWS: { key: keyof ClientProfilePoolView['builtin']; label: string }[] = [
  { key: 'app_version', label: '客户端版本' },
  { key: 'build_version', label: '构建号' },
  { key: 'version_code', label: 'TTNet 版本码' },
  { key: 'ide_version_code', label: 'IDE 版本码' },
  { key: 'package_type', label: '包类型' },
  { key: 'app_id', label: 'appId' },
  { key: 'os_name', label: '系统名' },
  { key: 'os_version', label: '系统版本' },
  { key: 'device_model', label: '设备型号' },
  { key: 'device_manufacturer', label: '设备厂商' },
  { key: 'cpu', label: 'CPU 厂商' },
];

/** 合成身份标记（下拉默认项） */
const SYNTHETIC = '';

export default function ClientProfilePanel() {
  const toast = useAppStore((s) => s.pushToast);
  const [view, setView] = useState<ClientProfilePoolView | null>(null);
  const [accounts, setAccounts] = useState<AccountView[]>([]);
  const [content, setContent] = useState('');
  const [loading, setLoading] = useState(true);
  const [busy, setBusy] = useState(false);
  const [confirmClear, setConfirmClear] = useState(false);
  const [pendingDelete, setPendingDelete] = useState<string | null>(null);
  const clearTimer = useRef<ReturnType<typeof setTimeout> | null>(null);
  const deleteTimer = useRef<ReturnType<typeof setTimeout> | null>(null);
  const fileRef = useRef<HTMLInputElement | null>(null);

  useEffect(
    () => () => {
      if (clearTimer.current) clearTimeout(clearTimer.current);
      if (deleteTimer.current) clearTimeout(deleteTimer.current);
    },
    [],
  );

  const load = useCallback(async () => {
    setLoading(true);
    try {
      const [v, list] = await Promise.all([api.clientProfile.get(), api.accounts.list()]);
      setView(v);
      setAccounts(list);
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
      setView(await api.clientProfile.import(text));
      setContent('');
      toast('success', '已加入设备池；在下方账号列表里绑定该设备后生效');
    } catch (e) {
      toast('error', `导入失败：${e}`);
    } finally {
      setBusy(false);
    }
  };

  const doBind = async (uid: string, deviceId: string) => {
    setBusy(true);
    try {
      setView(
        deviceId === SYNTHETIC
          ? await api.clientProfile.unbind(uid)
          : await api.clientProfile.bind(uid, deviceId),
      );
      toast('success', deviceId === SYNTHETIC ? '已改回合成身份' : '已绑定该真机设备');
    } catch (e) {
      toast('error', `绑定失败：${e}`);
    } finally {
      setBusy(false);
    }
  };

  const doDelete = async (deviceId: string) => {
    setBusy(true);
    try {
      if (deleteTimer.current) clearTimeout(deleteTimer.current);
      deleteTimer.current = null;
      setPendingDelete(null);
      setView(await api.clientProfile.delete(deviceId));
      toast('success', '已从设备池删除（相关账号绑定一并解除）');
    } catch (e) {
      toast('error', `删除失败：${e}`);
    } finally {
      setBusy(false);
    }
  };

  const doClear = async () => {
    setBusy(true);
    try {
      setView(await api.clientProfile.clear());
      toast('success', '已清空设备池，全部账号回退合成身份');
    } catch (e) {
      toast('error', `清空失败：${e}`);
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

  const devices = view?.devices ?? [];
  const bindings = view?.bindings ?? {};
  const boundCount = Object.keys(bindings).length;

  return (
    <div className="space-y-4">
      <div className="flex items-start gap-2 rounded-md border border-slate-200 p-3 text-xs text-slate-500 dark:border-zinc-700 dark:text-zinc-400">
        <Fingerprint size={15} className="mt-0.5 shrink-0" />
        <div>
          <p className="text-sm font-medium text-slate-700 dark:text-zinc-200">
            实机环境设备池（账号级绑定）
          </p>
          <p className="mt-1 opacity-80">
            在装有 Trae CN 的真机上运行导出脚本，把生成的 JSON 导入本网关形成设备池，
            再把具体账号绑定到某台真机。绑定后该账号的版本身份、设备指纹、device_id/machine_id
            全部以真机为准，登录、推理、模型同步三路身份完全一致。
          </p>
          <p className="mt-1 opacity-80">
            未绑定账号走派生/合成身份，不会与已绑定账号共用设备——多账号共用一台设备正是
            上游风控识别的典型特征。已有 device_map 绑定的账号不受影响（改了会触发上游
            Token device 不匹配）。
          </p>
        </div>
      </div>

      {/* 设备池 */}
      <div className="space-y-2">
        <div className="flex items-center gap-2">
          <span className="text-sm font-medium">设备池</span>
          <Badge tone={devices.length ? 'blue' : 'slate'}>{devices.length} 台</Badge>
          <Badge tone={boundCount ? 'green' : 'slate'}>{boundCount} 个账号已绑定</Badge>
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
              disabled={busy || devices.length === 0}
              className="flex items-center gap-1 rounded-md border border-slate-200 px-2.5 py-1.5 text-xs text-slate-600 transition hover:bg-slate-50 disabled:opacity-40 dark:border-zinc-700 dark:text-zinc-300 dark:hover:bg-zinc-800"
            >
              <RotateCcw size={13} />
              {confirmClear ? '确认清空？' : '清空设备池'}
            </button>
          </div>
        </div>

        {devices.length === 0 ? (
          <p className="rounded-md border border-dashed border-slate-200 px-3 py-6 text-center text-xs text-slate-400 dark:border-zinc-700">
            设备池为空，全部账号使用合成身份
          </p>
        ) : (
          <div className="overflow-hidden rounded-md border border-slate-200 dark:border-zinc-700">
            <table className="w-full text-xs">
              <thead className="bg-slate-50 text-slate-500 dark:bg-zinc-800/60 dark:text-zinc-400">
                <tr>
                  <th className="px-3 py-1.5 text-left font-medium">device_id</th>
                  <th className="px-3 py-1.5 text-left font-medium">客户端版本</th>
                  <th className="px-3 py-1.5 text-left font-medium">设备型号</th>
                  <th className="px-3 py-1.5 text-left font-medium">系统</th>
                  <th className="px-3 py-1.5 text-left font-medium">私钥</th>
                  <th className="px-3 py-1.5 text-left font-medium">已绑定账号</th>
                  <th className="w-16 px-3 py-1.5" />
                </tr>
              </thead>
              <tbody>
                {devices.map((d) => (
                  <tr
                    key={d.device_id}
                    className="border-t border-slate-100 dark:border-zinc-800"
                  >
                    <td className="px-3 py-1.5 font-mono break-all text-slate-700 dark:text-zinc-200">
                      {d.device_id}
                    </td>
                    <td className="px-3 py-1.5 text-slate-600 dark:text-zinc-300">
                      {d.app_version || '—'}
                    </td>
                    <td className="px-3 py-1.5 text-slate-600 dark:text-zinc-300">
                      {d.device_model || '—'}
                    </td>
                    <td className="px-3 py-1.5 text-slate-600 dark:text-zinc-300">
                      {d.os_version || '—'}
                    </td>
                    <td className="px-3 py-1.5">
                      <Badge tone={d.has_private_key ? 'green' : 'amber'}>
                        {d.has_private_key ? '有' : '无'}
                      </Badge>
                    </td>
                    <td className="px-3 py-1.5 text-slate-600 dark:text-zinc-300">
                      {d.bound_uids.length ? d.bound_uids.length : '—'}
                    </td>
                    <td className="px-3 py-1.5 text-right">
                      <button
                        onClick={() => {
                          if (deleteTimer.current) clearTimeout(deleteTimer.current);
                          if (pendingDelete === d.device_id) {
                            void doDelete(d.device_id);
                          } else {
                            setPendingDelete(d.device_id);
                            deleteTimer.current = setTimeout(() => {
                              setPendingDelete(null);
                              deleteTimer.current = null;
                            }, 3000);
                          }
                        }}
                        disabled={busy}
                        className="flex items-center gap-1 rounded-md border border-slate-200 px-2 py-1 text-slate-600 transition hover:bg-slate-50 disabled:opacity-40 dark:border-zinc-700 dark:text-zinc-300 dark:hover:bg-zinc-800"
                      >
                        <Trash2 size={12} />
                        {pendingDelete === d.device_id ? '确认？' : '删除'}
                      </button>
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        )}
      </div>

      {/* 账号绑定 */}
      <div className="space-y-2">
        <div className="flex items-center gap-2">
          <span className="text-sm font-medium">账号绑定</span>
          <span className="text-xs text-slate-400">
            未绑定 = 合成身份（按 uid 派生专属设备）
          </span>
        </div>
        <div className="overflow-hidden rounded-md border border-slate-200 dark:border-zinc-700">
          <table className="w-full text-xs">
            <thead className="bg-slate-50 text-slate-500 dark:bg-zinc-800/60 dark:text-zinc-400">
              <tr>
                <th className="px-3 py-1.5 text-left font-medium">账号</th>
                <th className="px-3 py-1.5 text-left font-medium">uid</th>
                <th className="w-72 px-3 py-1.5 text-left font-medium">设备</th>
              </tr>
            </thead>
            <tbody>
              {accounts.length === 0 && (
                <tr>
                  <td colSpan={3} className="px-3 py-6 text-center text-slate-400">
                    暂无账号
                  </td>
                </tr>
              )}
              {accounts.map((a) => {
                const bound = bindings[a.user_id];
                return (
                  <tr key={a.user_id} className="border-t border-slate-100 dark:border-zinc-800">
                    <td className="px-3 py-1.5 text-slate-700 dark:text-zinc-200">
                      {a.name || '未命名'}
                    </td>
                    <td className="px-3 py-1.5 font-mono break-all text-slate-500 dark:text-zinc-400">
                      {a.user_id}
                    </td>
                    <td className="px-3 py-1.5">
                      <div className="flex items-center gap-2">
                        <select
                          value={bound ?? SYNTHETIC}
                          disabled={busy}
                          onChange={(e) => void doBind(a.user_id, e.target.value)}
                          className="w-full rounded-md border border-slate-200 bg-transparent px-2 py-1 text-xs outline-none focus:border-brand-500 disabled:opacity-40 dark:border-zinc-700"
                        >
                          <option value={SYNTHETIC}>合成身份（默认）</option>
                          {devices.map((d, i) => (
                            <option key={d.device_id} value={d.device_id}>
                              真机 #{i + 1} · {d.device_id}
                            </option>
                          ))}
                        </select>
                        {bound ? (
                          <Link2 size={13} className="shrink-0 text-green-500" />
                        ) : (
                          <Unlink size={13} className="shrink-0 text-slate-300 dark:text-zinc-600" />
                        )}
                      </div>
                    </td>
                  </tr>
                );
              })}
            </tbody>
          </table>
        </div>
      </div>

      {/* 未绑定账号实际使用的内置身份 */}
      <div className="space-y-2">
        <span className="text-sm font-medium flex items-center gap-2">
          合成身份
          <Badge tone="slate">内置</Badge>
        </span>
        <div className="overflow-hidden rounded-md border border-slate-200 dark:border-zinc-700">
          <table className="w-full text-xs">
            <tbody>
              {BUILTIN_ROWS.map((r) => (
                <tr
                  key={r.key}
                  className="border-b border-slate-100 last:border-b-0 dark:border-zinc-800"
                >
                  <td className="w-40 px-3 py-1.5 text-slate-500 dark:text-zinc-400">{r.label}</td>
                  <td className="px-3 py-1.5 font-mono break-all text-slate-700 dark:text-zinc-200">
                    {view?.builtin[r.key] || (
                      <span className="text-slate-300 dark:text-zinc-600">—</span>
                    )}
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
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
              加入设备池
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
          按 device_id 去重；文件内 credential 段为 icube 密文，后端解密后仅存本地 kv，不回传前端、不写日志。
        </p>
      </div>
    </div>
  );
}
