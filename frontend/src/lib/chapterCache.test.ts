/**
 * chapterCache 单测 (node --test, 无第三方依赖).
 *
 * 仓库没装 fake-indexeddb, 这里只实现 chapterCache 实际用到的那一小撮 IndexedDB 接口
 * (open/onupgradeneeded/createObjectStore/createIndex/transaction/store.get|put|delete|
 * index().getAllKeys/IDBKeyRange.bound), 全部异步回调 (setTimeout) 以贴近真实时序。
 */
import assert from "node:assert/strict";
import test from "node:test";

type FakeKey = string | number;
type FakeKeyValue = FakeKey | FakeKey[];
type StoreKeyPath = string | string[];
type Record_ = Record<string, unknown>;

interface KeyRange {
  lower: FakeKeyValue;
  upper: FakeKeyValue;
}

function keyFrom(keyPath: StoreKeyPath, record: Record_): FakeKeyValue {
  if (Array.isArray(keyPath)) {
    return keyPath.map((part) => record[part] as FakeKey);
  }
  return record[keyPath] as FakeKey;
}

function compareKeys(a: FakeKeyValue, b: FakeKeyValue): number {
  const av = Array.isArray(a) ? a : [a];
  const bv = Array.isArray(b) ? b : [b];
  const len = Math.min(av.length, bv.length);
  for (let i = 0; i < len; i += 1) {
    const x = av[i] as FakeKey;
    const y = bv[i] as FakeKey;
    if (x < y) return -1;
    if (x > y) return 1;
  }
  return av.length - bv.length;
}

class FakeRequest<T> {
  result: T | undefined = undefined;
  error: unknown = null;
  onsuccess: (() => void) | null = null;
  onerror: (() => void) | null = null;
  onupgradeneeded: (() => void) | null = null;
  onblocked: (() => void) | null = null;
}

interface StoreDef {
  keyPath: StoreKeyPath;
  indexes: Map<string, StoreKeyPath>;
  records: Map<string, Record_>;
}

class FakeTransaction {
  error: unknown = null;
  oncomplete: (() => void) | null = null;
  onerror: (() => void) | null = null;
  onabort: (() => void) | null = null;
  private pending = 0;
  private opSeq = 0;
  private armedSeq = -1;
  private done = false;
  private readonly db: FakeDatabase;

  constructor(db: FakeDatabase) {
    this.db = db;
  }

  objectStore(name: string): FakeStore {
    return new FakeStore(this, this.db.def(name));
  }

  /** 排队一个操作: 结果与回调都异步回填, 事务在「没有新操作」后 complete */
  schedule<T>(apply: () => T): FakeRequest<T> {
    const request = new FakeRequest<T>();
    this.pending += 1;
    this.opSeq += 1;
    setTimeout(() => {
      try {
        request.result = apply();
      } catch (error) {
        request.error = error;
        this.error = error;
        this.pending -= 1;
        request.onerror?.();
        this.abortTx();
        return;
      }
      this.pending -= 1;
      request.onsuccess?.();
      this.armComplete();
    }, 0);
    return request;
  }

  private armComplete(): void {
    if (this.done || this.pending > 0) {
      return;
    }
    this.armedSeq = this.opSeq;
    setTimeout(() => {
      if (this.done) {
        return;
      }
      if (this.pending === 0 && this.opSeq === this.armedSeq) {
        this.done = true;
        this.oncomplete?.();
        return;
      }
      this.armComplete();
    }, 1);
  }

  private abortTx(): void {
    if (this.done) {
      return;
    }
    this.done = true;
    setTimeout(() => {
      this.onabort?.();
    }, 0);
  }
}

class FakeStore {
  private readonly tx: FakeTransaction;
  private readonly def: StoreDef;

  constructor(tx: FakeTransaction, def: StoreDef) {
    this.tx = tx;
    this.def = def;
  }

  get(key: FakeKeyValue): FakeRequest<Record_ | undefined> {
    return this.tx.schedule(() => this.def.records.get(JSON.stringify(key)));
  }

  put(record: Record_): FakeRequest<FakeKeyValue> {
    return this.tx.schedule(() => {
      const key = keyFrom(this.def.keyPath, record);
      this.def.records.set(JSON.stringify(key), record);
      return key;
    });
  }

  delete(key: FakeKeyValue): FakeRequest<undefined> {
    return this.tx.schedule(() => {
      this.def.records.delete(JSON.stringify(key));
      return undefined;
    });
  }

  index(name: string): { getAllKeys: (range?: KeyRange) => FakeRequest<FakeKeyValue[]> } {
    const indexKeyPath = this.def.indexes.get(name);
    if (indexKeyPath === undefined) {
      throw new Error(`missing index: ${name}`);
    }
    return {
      getAllKeys: (range?: KeyRange) =>
        this.tx.schedule(() => {
          const entries = [...this.def.records.values()]
            .map((record) => ({
              primary: keyFrom(this.def.keyPath, record),
              index: keyFrom(indexKeyPath, record),
            }))
            .filter(
              (entry) =>
                range === undefined ||
                (compareKeys(entry.index, range.lower) >= 0 &&
                  compareKeys(entry.index, range.upper) <= 0),
            )
            .sort((a, b) => compareKeys(a.index, b.index));
          return entries.map((entry) => entry.primary);
        }),
    };
  }
}

class FakeDatabase {
  readonly objectStoreNames: { contains: (name: string) => boolean };
  private readonly stores = new Map<string, StoreDef>();

  constructor() {
    this.objectStoreNames = { contains: (name: string) => this.stores.has(name) };
  }

  def(name: string): StoreDef {
    const found = this.stores.get(name);
    if (found === undefined) {
      throw new Error(`missing store: ${name}`);
    }
    return found;
  }

  createObjectStore(name: string, options: { keyPath: StoreKeyPath }): {
    createIndex: (indexName: string, keyPath: StoreKeyPath) => void;
  } {
    const def: StoreDef = { keyPath: options.keyPath, indexes: new Map(), records: new Map() };
    this.stores.set(name, def);
    return {
      createIndex: (indexName: string, keyPath: StoreKeyPath) => {
        def.indexes.set(indexName, keyPath);
      },
    };
  }

  transaction(names: string | string[]): FakeTransaction {
    const list = Array.isArray(names) ? names : [names];
    for (const name of list) {
      this.def(name);
    }
    return new FakeTransaction(this);
  }

  close(): void {
    // 真实实现里 close 只是断开引用, 这里无事可做
  }
}

type OpenMode = "ok" | "throw" | "error";

class FakeIndexedDb {
  openCalls = 0;
  mode: OpenMode = "ok";
  private readonly databases = new Map<string, FakeDatabase>();

  open(name: string): FakeRequest<FakeDatabase> {
    this.openCalls += 1;
    if (this.mode === "throw") {
      // 隐私模式/被策略禁用: 真实实现里可能同步抛 SecurityError
      throw new Error("SecurityError: IndexedDB is not available");
    }
    const request = new FakeRequest<FakeDatabase>();
    const mode = this.mode;
    setTimeout(() => {
      if (mode === "error") {
        request.error = new Error("open failed");
        request.onerror?.();
        return;
      }
      let db = this.databases.get(name);
      const isNew = db === undefined;
      if (db === undefined) {
        db = new FakeDatabase();
        this.databases.set(name, db);
      }
      request.result = db;
      if (isNew) {
        request.onupgradeneeded?.();
      }
      request.onsuccess?.();
    }, 0);
    return request;
  }
}

const fakeIdb = new FakeIndexedDb();
Object.defineProperty(globalThis, "indexedDB", { value: fakeIdb, configurable: true, writable: true });
Object.defineProperty(globalThis, "IDBKeyRange", {
  value: {
    bound: (lower: FakeKeyValue, upper: FakeKeyValue): KeyRange => ({ lower, upper }),
  },
  configurable: true,
  writable: true,
});

const cache = await import("./chapterCache.ts");

type ChapterList = Parameters<typeof cache.putChapters>[1];

function chaptersOf(prefix: string): ChapterList {
  return [
    { url: `${prefix}/1`, title: `${prefix} 第一章` },
    { url: `${prefix}/2`, title: `${prefix} 第二章` },
  ] as unknown as ChapterList;
}

function contentOf(text: string): Parameters<typeof cache.putContent>[2] {
  return { title: "t", content: text, nextContentUrl: "" } as unknown as Parameters<
    typeof cache.putContent
  >[2];
}

/** 每个用例前面重置模块内状态, 让失败态/节流 Map 不互相污染 */
function reset(): void {
  cache.resetChapterCacheState();
  fakeIdb.mode = "ok";
  fakeIdb.openCalls = 0;
}

test("目录: 写入后能读回, 未写过的书返回 undefined", async () => {
  reset();
  const chapters = chaptersOf("a");
  await cache.putChapters("book-a", chapters);
  assert.deepEqual(await cache.getChapters("book-a"), chapters);
  assert.equal(await cache.getChapters("book-missing"), undefined);
});

test("正文: 写入后能读回; chapterUrl 不符视为未命中", async () => {
  reset();
  await cache.putChapters("book-b", chaptersOf("b"));
  await cache.putContent("book-b", 1, contentOf("正文 B1"), "b/1");
  const hit = await cache.getContent("book-b", 1, "b/1");
  assert.equal(hit?.content, "正文 B1");
  assert.equal(hit?.chapterUrl, "b/1");
  // 换源后同一个 index 指向别的章: 不能串章
  assert.equal(await cache.getContent("book-b", 1, "other/1"), undefined);
  // 不传 chapterUrl 时只按 index 命中
  assert.equal((await cache.getContent("book-b", 1))?.content, "正文 B1");
});

test("版本失效: 目录指纹变了就丢弃该书已缓存正文", async () => {
  reset();
  await cache.putChapters("book-c", chaptersOf("c"));
  await cache.putContent("book-c", 1, contentOf("旧正文"), "c/1");
  assert.notEqual(await cache.getContent("book-c", 1, "c/1"), undefined);
  // 目录变了 (换源/大改): 正文的 index 对应关系失效
  await cache.putChapters("book-c", [
    { url: "c/new-1", title: "改过的第一章" },
  ] as unknown as ChapterList);
  assert.equal(await cache.getContent("book-c", 1, "c/1"), undefined);
  assert.deepEqual(await cache.getChapters("book-c"), [
    { url: "c/new-1", title: "改过的第一章" },
  ]);
});

test("淘汰: 目录只留最近 20 本, 每本正文只留最近 20 章", async () => {
  reset();
  for (let i = 0; i < 25; i += 1) {
    // 让 ts 单调递增, 保证淘汰顺序确定
    await new Promise((resolve) => setTimeout(resolve, 2));
    await cache.putChapters(`book-${i}`, chaptersOf(`p${i}`));
  }
  assert.equal(await cache.getChapters("book-0"), undefined, "最旧的 5 本目录应被淘汰");
  assert.notEqual(await cache.getChapters("book-24"), undefined, "最新一本应保留");

  for (let index = 0; index < 25; index += 1) {
    await new Promise((resolve) => setTimeout(resolve, 2));
    await cache.putContent("book-24", index, contentOf(`正文 ${index}`), `p24/${index}`);
  }
  assert.equal(await cache.getContent("book-24", 0, "p24/0"), undefined, "最旧的 5 章应被淘汰");
  assert.equal((await cache.getContent("book-24", 24, "p24/24"))?.content, "正文 24");
});

test("open 抛错时优雅降级: 读写都返回 undefined/不抛, 失败态被记住不再重复 open", async () => {
  reset();
  fakeIdb.mode = "throw";
  // 不抛 (降级成「无缓存」), 且不阻塞
  assert.equal(await cache.getChapters("book-x"), undefined);
  await cache.putChapters("book-x", chaptersOf("x"));
  assert.equal(await cache.getContent("book-x", 1), undefined);
  assert.equal(fakeIdb.openCalls, 1, "第一次失败后应记住失败态");

  // 冷却窗口内的后续调用必须直接降级, 不再碰 indexedDB.open
  assert.equal(await cache.getChapters("book-x"), undefined);
  await cache.putContent("book-x", 1, contentOf("正文"), "x/1");
  assert.equal(fakeIdb.openCalls, 1, "冷却期内不应再 open (否则隐私模式下每次进书都重复报错)");

  // 冷却到期 (这里靠重置模块状态模拟) 后允许再试一次: 临时故障可自愈
  cache.resetChapterCacheState();
  fakeIdb.mode = "ok";
  await cache.putChapters("book-x", chaptersOf("x"));
  assert.notEqual(await cache.getChapters("book-x"), undefined);
});
