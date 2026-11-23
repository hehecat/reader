import type { BookChapter } from "@/api/types";

const DB_NAME = "mojuan_v2_cache";
const DB_VERSION = 1;
const STORE_TOC = "toc";
const STORE_CHAPTER = "chapter";

let dbPromise: Promise<IDBDatabase | null> | null = null;

function openDb(): Promise<IDBDatabase | null> {
  if (typeof indexedDB === "undefined") return Promise.resolve(null);
  if (dbPromise) return dbPromise;
  dbPromise = new Promise((resolve) => {
    try {
      const req = indexedDB.open(DB_NAME, DB_VERSION);
      req.onupgradeneeded = () => {
        const db = req.result;
        if (!db.objectStoreNames.contains(STORE_TOC)) db.createObjectStore(STORE_TOC);
        if (!db.objectStoreNames.contains(STORE_CHAPTER)) db.createObjectStore(STORE_CHAPTER);
      };
      req.onsuccess = () => resolve(req.result);
      req.onerror = () => resolve(null);
    } catch {
      resolve(null);
    }
  });
  return dbPromise;
}

export async function getCachedToc(bookUrl: string): Promise<BookChapter[] | null> {
  const db = await openDb();
  if (!db) return null;
  return new Promise((resolve) => {
    try {
      const tx = db.transaction(STORE_TOC, "readonly");
      const req = tx.objectStore(STORE_TOC).get(bookUrl);
      req.onsuccess = () => resolve((req.result as BookChapter[] | undefined) ?? null);
      req.onerror = () => resolve(null);
    } catch {
      resolve(null);
    }
  });
}

export async function setCachedToc(bookUrl: string, chapters: BookChapter[]): Promise<void> {
  const db = await openDb();
  if (!db) return;
  try {
    const tx = db.transaction(STORE_TOC, "readwrite");
    tx.objectStore(STORE_TOC).put(chapters, bookUrl);
  } catch {
    // ignore
  }
}

export async function getCachedChapter(key: string): Promise<string | null> {
  const db = await openDb();
  if (!db) return null;
  return new Promise((resolve) => {
    try {
      const tx = db.transaction(STORE_CHAPTER, "readonly");
      const req = tx.objectStore(STORE_CHAPTER).get(key);
      req.onsuccess = () => resolve((req.result as string | undefined) ?? null);
      req.onerror = () => resolve(null);
    } catch {
      resolve(null);
    }
  });
}

export async function setCachedChapter(key: string, content: string): Promise<void> {
  const db = await openDb();
  if (!db) return;
  try {
    const tx = db.transaction(STORE_CHAPTER, "readwrite");
    tx.objectStore(STORE_CHAPTER).put(content, key);
  } catch {
    // ignore
  }
}
