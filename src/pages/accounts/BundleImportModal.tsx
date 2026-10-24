import { useEffect, useState } from 'react';
import { KeyRound, ShieldAlert } from 'lucide-react';
import { Modal } from '../../components/ui';

/**
 * 迁移包导入：文件已在父组件解析完成，此处只负责输入导出时设置的口令。
 * 迁移包内含账号凭证，因此不做明文展示、不落 localStorage。
 */
export function BundleImportModal({
  open,
  fileName,
  busy,
  onClose,
  onConfirm,
}: {
  open: boolean;
  fileName: string;
  busy: boolean;
  onClose: () => void;
  onConfirm: (password: string) => void;
}) {
  const [password, setPassword] = useState('');
  const [confirm, setConfirm] = useState('');

  useEffect(() => {
    if (open) {
      setPassword('');
      setConfirm('');
    }
  }, [open]);

  const tooShort = password.trim().length < 8;
  const mismatch = confirm.length > 0 && password !== confirm;
  const canSubmit = !busy && !tooShort && password === confirm;

  return (
    <Modal
      open={open}
      onClose={() => {
        if (!busy) onClose();
      }}
      title="导入迁移包"
      size="lg"
      footer={
        <>
          <button onClick={onClose} disabled={busy} className="btn-ghost">
            取消
          </button>
          <button
            onClick={() => onConfirm(password)}
            disabled={!canSubmit}
            className="btn-primary"
          >
            {busy ? '导入中…' : '解密并导入'}
          </button>
        </>
      }
    >
      <div className="space-y-4">
        <div className="flex items-start gap-2 rounded-lg border border-amber-200 bg-amber-50 px-3 py-2 text-xs text-amber-700 dark:border-amber-500/30 dark:bg-amber-500/10 dark:text-amber-300">
          <ShieldAlert size={16} className="mt-0.5 shrink-0" />
          <span>
            迁移包内含账号凭证，等同密码。仅从可信设备导入，导入完成后请妥善保管或删除源文件。
          </span>
        </div>

        <div className="text-sm">
          <span className="text-slate-500 dark:text-slate-400">文件：</span>
          <span className="break-all font-mono text-xs">{fileName || '-'}</span>
        </div>

        <label className="block space-y-1.5">
          <span className="flex items-center gap-1.5 text-sm font-medium">
            <KeyRound size={15} /> 导出时设置的口令
          </span>
          <input
            type="password"
            value={password}
            onChange={(e) => setPassword(e.target.value)}
            autoFocus
            placeholder="至少 8 个字符"
            className="input w-full"
          />
          {tooShort && password.length > 0 && (
            <span className="text-xs text-amber-600 dark:text-amber-400">口令至少 8 个字符</span>
          )}
        </label>

        <label className="block space-y-1.5">
          <span className="text-sm font-medium">确认口令</span>
          <input
            type="password"
            value={confirm}
            onChange={(e) => setConfirm(e.target.value)}
            placeholder="再次输入"
            className="input w-full"
          />
          {mismatch && (
            <span className="text-xs text-rose-600 dark:text-rose-400">两次输入的口令不一致</span>
          )}
        </label>

        <p className="text-xs text-slate-400 dark:text-slate-500">
          导入会按 uid 自动去重并合并到对应账号池；已存在的账号会更新凭证，不会重复添加。
        </p>
      </div>
    </Modal>
  );
}
