import { closeChapterCache } from "@/lib/chapterCache";
import { queryClient } from "@/lib/query-client";
import { ANNOTATIONS_STORAGE_KEY, useAnnotationsStore } from "@/stores/annotations-store";
import { SETTINGS_STORAGE_KEY, defaultSettings, useSettingsStore } from "@/stores/settings-store";

/**
 * 本地存储的用户维度隔离.
 *
 * 模型: live 键(`reader.settings` 等)始终代表「当前登录用户」; 登录/登出时把 live 键整体
 * 搬进 `reader.u.<user>.<key>` 桶, 再把目标用户的桶搬回 live. 于是所有既有调用点
 * (12+ 处, 分散在 store/hook/组件里)一行都不用改, 以后新增的 `reader.*` 键也自动被隔离.
 *
 * 为什么是「搬家」而不是给每个调用点加前缀: 调用点太多, 逐个加前缀既漏又难维护;
 * 搬家只需要在登录态切换的两个入口各调一次.
 */

const BUCKET_PREFIX = "reader.u.";
/** 访客(未登录)的桶名: 空用户名不能直接拼进 key */
const ANON = "_";
/** 跨用户共享的键: token 是身份本身; secureKey 存 sessionStorage, 这里只做兜底排除 */
const EXCLUDED = new Set(["reader.accessToken", "reader.secureKey"]);
/**
 * live 键当前归属哪个桶.
 * 401 路径里 api-client 会先 clearAccessToken() 再触发 logout(), 那一刻只靠 token
 * 已经反推不出上一个用户是谁; 没有这个 marker, A 的数据会被归档进访客桶, 下次匿名访问就能读到.
 */
const ACTIVE_KEY = "reader.u.active";

/** 隐私模式下 localStorage 访问会抛, 统一降级为 null */
function storage(): Storage | null {
  try {
    return window.localStorage;
  } catch {
    return null;
  }
}

/** 当前登录用户名: 取自 localStorage 的 `username:token` 冒号前段 */
export function currentUsername(): string | null {
  try {
    const raw = storage()?.getItem("reader.accessToken") ?? null;
    if (raw === null) {
      return null;
    }
    const separator = raw.indexOf(":");
    return separator > 0 ? raw.slice(0, separator) : null;
  } catch {
    return null;
  }
}

function bucketOf(username: string | null): string {
  return username === null || username === "" ? ANON : username;
}

/** live 键的真实归属: token 优先, token 已被清掉时退回 marker */
function previousOwner(st: Storage, prev: string | null): string | null {
  if (prev !== null && prev !== "") {
    return prev;
  }
  try {
    const marked = st.getItem(ACTIVE_KEY);
    return marked === null || marked === ANON ? null : marked;
  } catch {
    return null;
  }
}

/** live 键: `reader.` 开头、不是共享键、也不是桶内的键 */
function liveKeys(st: Storage): string[] {
  const keys: string[] = [];
  for (let i = 0; i < st.length; i += 1) {
    const key = st.key(i);
    if (key === null || !key.startsWith("reader.")) {
      continue;
    }
    if (EXCLUDED.has(key) || key.startsWith(BUCKET_PREFIX)) {
      continue;
    }
    keys.push(key);
  }
  return keys;
}

/** 指定桶内的键(先取快照, 之后边搬边删不会打乱遍历) */
function keysWithPrefix(st: Storage, prefix: string): string[] {
  const keys: string[] = [];
  for (let i = 0; i < st.length; i += 1) {
    const key = st.key(i);
    if (key !== null && key.startsWith(prefix)) {
      keys.push(key);
    }
  }
  return keys;
}

/**
 * 把 live 的 `reader.*` 键搬进 prev 用户桶, 再把 next 用户桶的键搬回 live.
 *
 * 调用时机: 登录成功(prev=登录前用户, next=新用户)、登出(prev=当前用户, next=null).
 * 首次访问(prev/next 都是 null)直接返回, 不做任何事 —— 新装/未登录时 live 键本来就属于访客.
 */
export function swapUserData(prev: string | null, next: string | null): void {
  try {
    const st = window.localStorage;
    const from = bucketOf(previousOwner(st, prev));
    const to = bucketOf(next);
    // live 键已经属于同一个桶(重复登录同一账号/匿名态下登出): 搬家只会白写一遍
    if (from === to) {
      return;
    }
    for (const key of liveKeys(st)) {
      const value = st.getItem(key);
      if (value === null) {
        continue;
      }
      st.setItem(`${BUCKET_PREFIX}${from}.${key}`, value);
      st.removeItem(key);
    }
    const prefix = `${BUCKET_PREFIX}${to}.`;
    for (const key of keysWithPrefix(st, prefix)) {
      const live = key.slice(prefix.length);
      const value = st.getItem(key);
      if (value === null) {
        continue;
      }
      // 半途失败可能留下同名 live 键: 先清掉, 免得把上一个用户的值混进目标用户的数据里
      st.removeItem(live);
      st.setItem(live, value);
      st.removeItem(key);
    }
    st.setItem(ACTIVE_KEY, to);
  } catch {
    // 配额满/隐私模式: 放弃搬家, 最坏等于隔离前的行为, 但登录本身不能因此失败
  }
}

function hasLive(storageKey: string): boolean {
  try {
    return window.localStorage.getItem(storageKey) !== null;
  } catch {
    return false;
  }
}

/**
 * 切换用户后重置运行时状态.
 *
 * 只搬 localStorage 不够: 内存里的 react-query 缓存和 zustand 状态还留着上一个用户的数据,
 * 换账号后不刷新页面就能看到别人的书架/账号列表/批注. IndexedDB 连接同理, 按用户分库后
 * 旧连接还连着旧库.
 */
export function resetUserRuntime(): void {
  // 不 clear 的话 A(admin) 打开过的 /users 列表在 SPA 内跳回时照样渲染给 B
  queryClient.clear();
  // persist.rehydrate() 在 storage 里没有对应键时会保留内存态: 新用户的桶是空的时候,
  // A 的批注/设置会留在 B 的界面上. 先复位成默认值, 让随后的 rehydrate 落在干净基线上
  // (这里写回的是默认值, 不会覆盖目标用户已经搬回 live 的真实数据).
  if (!hasLive(ANNOTATIONS_STORAGE_KEY)) {
    useAnnotationsStore.setState({ byBook: {} });
  }
  if (!hasLive(SETTINGS_STORAGE_KEY)) {
    useSettingsStore.setState(defaultSettings);
  }
  void useAnnotationsStore.persist.rehydrate();
  void useSettingsStore.persist.rehydrate();
  closeChapterCache();
}
