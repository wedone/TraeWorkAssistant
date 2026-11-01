import { useCallback, useEffect, useState } from 'react';
import { CalendarClock, RotateCcw, Loader2 } from 'lucide-react';
import { Badge, Spinner } from './ui';
import { api } from '../lib/tauri';
import { useAppStore } from '../store';
import type { SchedulerTaskView } from '../types';

/**
 * 定时任务配置卡（Trae / Buddy 环境配置共用）：
 * 按平台展示内置调度任务开关（kv scheduler_cfg.disabled_tasks，缺省全开 = 推荐配置）、
 * 可编辑的每日触发时刻（kv scheduler_cfg.task_times，空 = 默认时刻）、签到类每日随机
 * 延迟窗（kv scheduler_cfg.jitter_minutes，0~120 分钟，默认 30）与最近一次执行摘要；
 * wb-checkin 由外部 override 绑定 WorkBuddySettings.auto_checkin。
 */

/** 各任务一句话说明（key → 描述） */
const TASK_DESC: Record<string, string> = {
  'trae-jwt-renew': '临期 JWT 自动续期，剩余有效期 > 48h 时自动跳过',
  'models-sync': '同步官网模型列表（不消耗积分），供网关与模型选择器使用',
  'wb-catalog-sync': '同步 Buddy 上游模型目录（不消耗积分），供网关模型映射使用',
  'trae-checkin': '全账号每日自动签到，已签账号自动跳过（幂等）',
  'wb-checkin': '每日自动签到；服务启动后也会自动补签当日签到',
  'wb-growth': '每日成长任务（旅行/盲盒/任务三开关驱动，全关时空轮）',
  'wb-renew': 'Token 到期前 24h 内自动续期（兜底，每日检查一次）',
  'wb-credits-snapshot': '积分余额快照 + 官方用量刷新，补齐积分看板近 7 日消耗趋势',
  'trae-credits-snapshot': '积分余额每日快照，补齐积分看板消耗趋势',
  'trae-usage-sync': 'Trae 消耗明细同步，补齐看板会话明细数据',
  'qoder-checkin': '全账号每日自动签到（0 点签到 + 10 点登录奖励双活动），已领账号自动跳过',
  'qoder-refresh': '每 6 小时为全部账号续期登录凭证（固定周期，不受时刻配置影响）',
  'qoder-credits-snapshot': '积分余额每日快照，补齐积分看板消耗趋势',
  'qoder-catalog-sync': '同步 Qoder 上游模型目录（不消耗积分），供网关模型映射使用',
  'qoder-sessions-compact': '每 6 小时合并历史会话流水为聚合行（固定周期，保留近 3 天请求级明细）',
};

/** 可配置「每小时/每N小时」模式的任务（看板数据同步类，与服务端 HOURLY_CAPABLE 对齐） */
const HOURLY_TASKS = ['wb-credits-snapshot', 'trae-credits-snapshot', 'trae-usage-sync'];

/** 支持每日随机延迟的签到类任务（与服务端 JITTER_CAPABLE 对齐） */
const JITTER_TASKS = ['trae-checkin', 'wb-checkin', 'wb-growth', 'qoder-checkin', 'buddy-ai-checkin'];

/** 随机延迟窗缺省值与上限（分钟；与服务端 DEFAULT/MAX_JITTER_MINUTES 同口径） */
const JITTER_DEFAULT = 30;
const JITTER_MAX = 120;

/** 有活动时间窗的签到任务（Qoder 0 点签到 + 10 点登录奖励、Buddy 每日活跃打卡）：
 *  延迟上限收紧到 30 分钟，避免晚于活动窗口漏领 */
const JITTER_WINDOW_TASKS = ['qoder-checkin', 'buddy-ai-checkin'];

/** 任务生效的随机延迟上限（分钟） */
const jitterMaxOf = (key: string) => (JITTER_WINDOW_TASKS.includes(key) ? JITTER_DEFAULT : JITTER_MAX);

/** 外部接管开关（wb-checkin → settings.auto_checkin）：checked + 切换回调 */
export interface TaskToggleOverride {
  checked: boolean;
  onToggle: (v: boolean) => void;
  /** 切换请求进行中（禁用开关防连点） */
  busy?: boolean;
}

export default function SchedulerTasksCard({
  taskKeys,
  title = '定时任务',
  desc = '由服务端内置调度器每日自动执行，失败 30 分钟后自动重试。',
  overrides,
  className,
}: {
  taskKeys: string[];
  title?: string;
  desc?: string;
  /** 需要（部分开关）绑定到其他配置源时提供，如 wb-checkin → auto_checkin */
  overrides?: Record<string, TaskToggleOverride>;
  className?: string;
}) {
  const pushToast = useAppStore((s) => s.pushToast);
  const [tasks, setTasks] = useState<SchedulerTaskView[] | null>(null);
  const [disabled, setDisabled] = useState<string[]>([]);
  /** 自定义触发时刻表（key → HH:MM；空 = 默认时刻，状态接口回显生效值） */
  const [times, setTimes] = useState<Record<string, string>>({});
  /** 执行模式表（key → "daily" | "hourly" | "every_hours"；缺省 = every_hours，仅看板同步类任务可配） */
  const [modes, setModes] = useState<Record<string, string>>({});
  /** every_hours 间隔小时数（key → 1..24；缺省 2，状态接口回填生效值） */
  const [taskHours, setTaskHours] = useState<Record<string, number>>({});
  /** 间隔输入草稿（key → 原始字符串；保留清空等中间态，失焦时才数值化保存） */
  const [hourDrafts, setHourDrafts] = useState<Record<string, string>>({});
  /** 每日随机延迟窗（key → 0..120 分钟；缺省 30，状态接口回填生效值） */
  const [jitter, setJitter] = useState<Record<string, number>>({});
  /** 随机延迟输入草稿（key → 原始字符串；同 hourDrafts，失焦时才保存） */
  const [jitterDrafts, setJitterDrafts] = useState<Record<string, string>>({});
  const [toggling, setToggling] = useState<string | null>(null);
  const [timeSaving, setTimeSaving] = useState<string | null>(null);
  const [resetting, setResetting] = useState(false);

  const reload = useCallback(() => {
    api.scheduler
      .status()
      .then((r) => setTasks(r.tasks.filter((t) => taskKeys.includes(t.key))))
      .catch(() => setTasks(null));
    api.scheduler
      .configGet()
      .then((c) => {
        setDisabled(c.disabled_tasks);
        setTimes(c.task_times ?? {});
        setModes(c.task_modes ?? {});
        setTaskHours(c.task_hours ?? {});
        // 兼容全局整数形态：按签到类任务展开回显（服务端保存时同样落成 map）
        const jitterCfg = c.jitter_minutes ?? {};
        setJitter(
          typeof jitterCfg === 'number'
            ? Object.fromEntries(JITTER_TASKS.map((k) => [k, jitterCfg]))
            : jitterCfg,
        );
      })
      .catch(() => setDisabled([]));
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  useEffect(() => {
    reload();
  }, [reload]);

  /** 开关切换：乐观更新 → 服务端保存，失败回滚并提示 */
  const toggleTask = async (key: string, next: boolean) => {
    if (overrides?.[key]) {
      overrides[key].onToggle(next);
      return;
    }
    const prev = disabled;
    const nextList = next ? prev.filter((k) => k !== key) : [...prev, key];
    setDisabled(nextList);
    setToggling(key);
    try {
      // config_set 为整表替换：必须同时携带自定义时刻/执行模式/间隔小时数，否则会被清空
      const saved = await api.scheduler.configSet({ disabled_tasks: nextList, task_times: times, task_modes: modes, task_hours: taskHours, jitter_minutes: jitter });
      setDisabled(saved.disabled_tasks);
      setTimes(saved.task_times ?? {});
      pushToast('success', next ? '任务已启用，明日按计划执行' : '任务已停用');
      // enabled 合成了其他设置语义，重取状态保持展示准确
      api.scheduler
        .status()
        .then((r) => setTasks(r.tasks.filter((t) => taskKeys.includes(t.key))))
        .catch(() => {});
    } catch (e) {
      setDisabled(prev);
      pushToast('error', e instanceof Error ? e.message : '保存失败');
    } finally {
      setToggling(null);
    }
  };

  /** 修改执行时刻：清空 = 恢复默认；保存后重取状态回显生效时刻 */
  const saveTime = async (key: string, value: string) => {
    const prev = times;
    const next = { ...prev };
    if (value) next[key] = value;
    else delete next[key];
    setTimes(next);
    setTimeSaving(key);
    try {
      const saved = await api.scheduler.configSet({ disabled_tasks: disabled, task_times: next, task_modes: modes, task_hours: taskHours, jitter_minutes: jitter });
      setTimes(saved.task_times ?? {});
      pushToast('success', value ? `执行时间已改为 ${value}（当天已过新时刻会自动补跑）` : '已恢复默认执行时间');
      api.scheduler
        .status()
        .then((r) => setTasks(r.tasks.filter((t) => taskKeys.includes(t.key))))
        .catch(() => {});
    } catch (e) {
      setTimes(prev);
      pushToast('error', e instanceof Error ? e.message : '保存失败');
    } finally {
      setTimeSaving(null);
    }
  };

  /** 切换执行模式（每N小时/每日/每小时，仅看板同步类）：every_hours 每 N 小时节流 / hourly 每小时节流 */
  const saveMode = async (key: string, mode: string) => {
    const prev = modes;
    const next = { ...prev, [key]: mode };
    setModes(next);
    setToggling(key);
    try {
      const saved = await api.scheduler.configSet({ disabled_tasks: disabled, task_times: times, task_modes: next, task_hours: taskHours, jitter_minutes: jitter });
      setModes(saved.task_modes ?? {});
      pushToast(
        'success',
        mode === 'hourly'
          ? '已改为每小时执行（距上次成功 ≥1 小时才再跑）'
          : mode === 'every_hours'
            ? `已改为每 ${taskHours[key] ?? 2} 小时执行（距上次成功满间隔才再跑）`
            : '已改为每日定时执行',
      );
    } catch (e) {
      setModes(prev);
      pushToast('error', e instanceof Error ? e.message : '保存失败');
    } finally {
      setToggling(null);
    }
  };

  /** 修改 every_hours 间隔小时数（1..24，失焦归一，与后端 clamp 口径一致） */
  const saveHours = async (key: string, value: number) => {
    const n = Math.min(24, Math.max(1, Math.round(value) || 2));
    const prev = taskHours;
    const next = { ...prev, [key]: n };
    setTaskHours(next);
    setTimeSaving(key);
    try {
      const saved = await api.scheduler.configSet({ disabled_tasks: disabled, task_times: times, task_modes: modes, task_hours: next, jitter_minutes: jitter });
      setTaskHours(saved.task_hours ?? {});
      pushToast('success', `已改为每 ${n} 小时执行一次（含积分快照与消耗/会话明细）`);
      api.scheduler
        .status()
        .then((r) => setTasks(r.tasks.filter((t) => taskKeys.includes(t.key))))
        .catch(() => {});
    } catch (e) {
      setTaskHours(prev);
      pushToast('error', e instanceof Error ? e.message : '保存失败');
    } finally {
      setTimeSaving(null);
    }
  };

  /** 修改每日随机延迟窗（0..120，0 = 关闭，缺省 30；失焦时归一保存） */
  const saveJitter = async (key: string, value: number) => {
    const n = Math.min(jitterMaxOf(key), Math.max(0, Math.round(value) || 0));
    const prev = jitter;
    const next = { ...prev, [key]: n };
    setJitter(next);
    setTimeSaving(key);
    try {
      const saved = await api.scheduler.configSet({ disabled_tasks: disabled, task_times: times, task_modes: modes, task_hours: taskHours, jitter_minutes: next });
      if (saved.jitter_minutes && typeof saved.jitter_minutes === 'object') setJitter(saved.jitter_minutes);
      pushToast(
        'success',
        n > 0 ? `已设置每日随机延迟 0~${n} 分钟（当天已抽签的延迟保持不变）` : '已关闭该任务的每日随机延迟',
      );
    } catch (e) {
      setJitter(prev);
      pushToast('error', e instanceof Error ? e.message : '保存失败');
    } finally {
      setTimeSaving(null);
    }
  };

  /** 恢复推荐配置：清空停用名单与自定义时刻（全部启用 + 默认时刻） */
  const resetRecommended = async () => {
    setResetting(true);
    try {
      const saved = await api.scheduler.configSet({ disabled_tasks: [], task_times: {}, task_modes: {}, task_hours: {}, jitter_minutes: {} });
      setDisabled(saved.disabled_tasks);
      setTimes(saved.task_times ?? {});
      setModes(saved.task_modes ?? {});
      setTaskHours(saved.task_hours ?? {});
      setJitter(saved.jitter_minutes && typeof saved.jitter_minutes === 'object' ? saved.jitter_minutes : {});
      pushToast('success', '已恢复推荐配置（全部任务启用 + 默认时刻）');
      api.scheduler
        .status()
        .then((r) => setTasks(r.tasks.filter((t) => taskKeys.includes(t.key))))
        .catch(() => {});
    } catch (e) {
      pushToast('error', e instanceof Error ? e.message : '恢复失败');
    } finally {
      setResetting(false);
    }
  };

  // 恢复推荐按钮只覆盖本卡内的 kv 配置任务；外部 override 绑定的开关由自身行内开关恢复
  // （task_hours 回填生效值，仅 ≠2 视为自定义；modes 缺省 = every_hours 默认档，不算自定义）
  // （jitter 回填生效值，仅 ≠30 默认窗视为自定义）
  const hasCustom =
    disabled.some((k) => taskKeys.includes(k)) ||
    taskKeys.some(
      (k) =>
        times[k] ||
        modes[k] ||
        (taskHours[k] !== undefined && taskHours[k] !== 2) ||
        (JITTER_TASKS.includes(k) && jitter[k] !== undefined && jitter[k] !== JITTER_DEFAULT),
    );

  return (
    <section className={`card p-4 ${className ?? ''}`}>
      <div className="mb-1 flex flex-wrap items-center justify-between gap-2">
        <div className="flex items-center gap-2">
          <CalendarClock size={16} className="text-brand-500" />
          <h3 className="font-medium">{title}</h3>
        </div>
        {hasCustom && (
          <button onClick={resetRecommended} disabled={resetting} className="btn-outline text-xs">
            {resetting ? <Loader2 size={13} className="animate-spin" /> : <RotateCcw size={13} />} 恢复推荐配置
          </button>
        )}
      </div>
      <p className="mb-3 text-xs text-slate-400">{desc}</p>

      {!tasks ? (
        <div className="flex items-center gap-2 text-sm text-slate-400">
          <Spinner /> 加载中…
        </div>
      ) : (
        <div className="space-y-2">
          {taskKeys.map((key) => {
            const t = tasks.find((x) => x.key === key);
            const ov = overrides?.[key];
            const on = ov ? ov.checked : !disabled.includes(key);
            const busy = ov ? !!ov.busy : toggling === key;
            return (
              <div
                key={key}
                className={`flex items-start gap-3 rounded-lg bg-slate-50 px-3 py-2.5 transition-opacity dark:bg-zinc-900 ${
                  on ? '' : 'opacity-55'
                }`}
              >
                <input
                  type="checkbox"
                  className="mt-1"
                  checked={on}
                  disabled={busy}
                  onChange={(e) => void toggleTask(key, e.target.checked)}
                />
                <div className="min-w-0 flex-1">
                  <div className="flex flex-wrap items-center gap-2">
                    <span className="text-sm font-medium">{t?.name ?? key}</span>
                    {t && HOURLY_TASKS.includes(key) && (
                      <select
                        value={modes[key] ?? 'every_hours'}
                        disabled={toggling === key}
                        onChange={(e) => void saveMode(key, e.target.value)}
                        className="rounded-md border border-slate-200 bg-white px-1.5 py-0.5 text-xs text-slate-600 disabled:opacity-50 dark:border-zinc-700 dark:bg-zinc-800 dark:text-zinc-300"
                        title="执行模式：每 N 小时（默认 2h）/ 每日定时 / 每小时（距上次成功 ≥1 小时才再跑）"
                      >
                        <option value="every_hours">每N小时</option>
                        <option value="daily">每日</option>
                        <option value="hourly">每小时</option>
                      </select>
                    )}
                    {t &&
                      HOURLY_TASKS.includes(key) &&
                      (modes[key] ?? 'every_hours') === 'every_hours' && (
                        <input
                          type="number"
                          min={1}
                          max={24}
                          value={hourDrafts[key] ?? String(taskHours[key] ?? 2)}
                          disabled={timeSaving === key}
                          onChange={(e) => setHourDrafts({ ...hourDrafts, [key]: e.target.value })}
                          onBlur={() => {
                            // 审查修复：输入过程保留原始字符串（清空的中间态不再瞬时变 0），
                            // 失焦时丢弃草稿恢复已生效显示；空串/非数字不触发保存
                            const raw = (hourDrafts[key] ?? '').trim();
                            setHourDrafts((prev) => {
                              if (!(key in prev)) return prev;
                              const next = { ...prev };
                              delete next[key];
                              return next;
                            });
                            if (raw === '' || Number.isNaN(Number(raw))) return;
                            void saveHours(key, Number(raw));
                          }}
                          className="w-14 rounded-md border border-slate-200 bg-white px-1.5 py-0.5 text-xs tabular-nums text-slate-600 disabled:opacity-50 dark:border-zinc-700 dark:bg-zinc-800 dark:text-zinc-300"
                          title="间隔小时数（1-24，失焦保存；距上次成功满间隔才再跑）"
                        />
                      )}
                    {t && t.mode === 'every6h' ? (
                      <Badge tone="violet" title="固定周期任务：每 6 小时自动执行一次，触发时刻不可配置">
                        {t.time}
                      </Badge>
                    ) : (
                      t &&
                      !(HOURLY_TASKS.includes(key) && modes[key] !== 'daily') && (
                        <input
                          type="time"
                          value={t.time}
                          disabled={timeSaving === key}
                          onChange={(e) => void saveTime(key, e.target.value)}
                          className="w-[92px] rounded-md border border-slate-200 bg-white px-1.5 py-0.5 text-xs tabular-nums text-slate-600 disabled:opacity-50 dark:border-zinc-700 dark:bg-zinc-800 dark:text-zinc-300"
                          title="每日触发时刻（修改即时保存；留空恢复默认）"
                        />
                      )
                    )}
                    {t && JITTER_TASKS.includes(key) && (
                      <span className="flex items-center gap-1 text-xs text-slate-400">
                        <input
                          type="number"
                          min={0}
                          max={jitterMaxOf(key)}
                          value={jitterDrafts[key] ?? String(jitter[key] ?? JITTER_DEFAULT)}
                          disabled={timeSaving === key}
                          onChange={(e) => setJitterDrafts({ ...jitterDrafts, [key]: e.target.value })}
                          onBlur={() => {
                            // 同 hourDrafts：输入过程保留原始字符串，失焦归一后保存；
                            // 空串/非数字恢复已生效值，不触发保存
                            const raw = (jitterDrafts[key] ?? '').trim();
                            setJitterDrafts((prev) => {
                              if (!(key in prev)) return prev;
                              const next = { ...prev };
                              delete next[key];
                              return next;
                            });
                            if (raw === '' || Number.isNaN(Number(raw))) return;
                            void saveJitter(key, Number(raw));
                          }}
                          className="w-14 rounded-md border border-slate-200 bg-white px-1.5 py-0.5 text-xs tabular-nums text-slate-600 disabled:opacity-50 dark:border-zinc-700 dark:bg-zinc-800 dark:text-zinc-300"
                          title={`每日在设定时刻后 0~${jitterMaxOf(key)} 分钟内随机延迟执行，0=关闭（失焦保存，默认 30）`}
                        />
                        分钟随机延迟
                      </span>
                    )}
                    {times[key] && t?.time !== undefined && (
                      <button
                        onClick={() => void saveTime(key, '')}
                        disabled={timeSaving === key}
                        className="text-xs text-slate-400 underline-offset-2 hover:text-brand-500 hover:underline"
                        title="恢复该任务的默认时刻"
                      >
                        默认
                      </button>
                    )}
                    {!on && <Badge tone="amber">已停用</Badge>}
                  </div>
                  <p className="mt-0.5 text-xs text-slate-400">{TASK_DESC[key] ?? ''}</p>
                  {JITTER_TASKS.includes(key) && (
                    <p className="mt-0.5 text-xs text-slate-400">
                      每日在设定时刻后 0~{jitter[key] ?? JITTER_DEFAULT} 分钟内随机延迟执行，0=关闭
                    </p>
                  )}
                  {t?.last_summary && (
                    <p
                      className={`mt-1 truncate text-xs ${
                        t.last_ok === false ? 'text-rose-500' : 'text-emerald-600 dark:text-emerald-400'
                      }`}
                      title={`${t.last_run_date ?? ''} ${t.last_summary}`}
                    >
                      {t.last_ok === false ? '上次失败：' : '上次执行：'}
                      {t.last_summary}
                      {t.last_run_date ? `（${t.last_run_date}）` : ''}
                    </p>
                  )}
                </div>
              </div>
            );
          })}
        </div>
      )}
    </section>
  );
}
