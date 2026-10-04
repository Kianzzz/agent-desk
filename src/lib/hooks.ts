import { useCallback, useEffect, useRef, useState } from "react";
import { api, onEvent } from "../api";
import type { Account, DiskReport, ScanReport, Settings, UsageState } from "../types";

/** 读一次数据，并在指定事件到来时重读。 */
function useLive<T>(load: () => Promise<T>, events: string[]): [T | null, () => Promise<void>, string | null] {
  const [data, setData] = useState<T | null>(null);
  const [error, setError] = useState<string | null>(null);
  const loadRef = useRef(load);
  loadRef.current = load;
  const reload = useCallback(async () => {
    try {
      setData(await loadRef.current());
      setError(null);
    } catch (e) {
      setError(String(e));
    }
  }, []);
  useEffect(() => {
    reload();
    const offs = events.map((e) => onEvent(e, reload));
    return () => offs.forEach((off) => off());
  }, [reload, events.join(",")]);
  return [data, reload, error];
}

export const useUsage = () => useLive<UsageState>(api.getUsage, ["usage-updated"]);
export const useSecurity = () => useLive<ScanReport | null>(api.getSecurity, ["security-updated"]);
export const useDisk = () => useLive<DiskReport | null>(api.getDisk, ["disk-updated"]);
export const useSettings = () => useLive<Settings>(api.getSettings, ["settings-updated"]);

/** 账号和额度：用量刷新时重读，另外每 30 秒读一次（Claude 状态栏随时会写新数据）。 */
export function useAccounts(): Account[] | null {
  const [accounts, reload] = useLive<Account[]>(api.getAccounts, ["usage-updated"]);
  useEffect(() => {
    const t = setInterval(reload, 30000);
    return () => clearInterval(t);
  }, [reload]);
  return accounts;
}

/** 每隔 ms 触发一次重渲染，用于「3 分钟前」这类相对时间。 */
export function useTick(ms = 30000) {
  const [, setN] = useState(0);
  useEffect(() => {
    const t = setInterval(() => setN((n) => n + 1), ms);
    return () => clearInterval(t);
  }, [ms]);
}

/** 包一层异步操作：busy 状态和错误提示。 */
export function useAction<A extends unknown[], R>(fn: (...args: A) => Promise<R>) {
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const run = useCallback(
    async (...args: A): Promise<R | undefined> => {
      setBusy(true);
      setError(null);
      try {
        return await fn(...args);
      } catch (e) {
        setError(String(e));
        return undefined;
      } finally {
        setBusy(false);
      }
    },
    [fn],
  );
  return { run, busy, error, setError };
}
