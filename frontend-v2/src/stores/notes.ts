import { create } from "zustand";
import { createJSONStorage, persist } from "zustand/middleware";

export type InkColor = "amber" | "sage" | "vermilion";

export interface ReadingNote {
  id: string;
  bookUrl: string;
  chapterIndex: number;
  chapterTitle: string;
  paragraphIndex: number;
  quote: string;
  note: string;
  color: InkColor;
  createdAt: number;
}

interface NotesState {
  byBook: Record<string, ReadingNote[]>;
  addNote: (item: Omit<ReadingNote, "id" | "createdAt">) => ReadingNote;
  removeNote: (bookUrl: string, id: string) => void;
  clearBook: (bookUrl: string) => void;
}

export const useNotesStore = create<NotesState>()(
  persist(
    (set) => ({
      byBook: {},
      addNote: (item) => {
        const entry: ReadingNote = {
          ...item,
          id: `${Date.now()}-${Math.random().toString(36).slice(2, 7)}`,
          createdAt: Date.now(),
        };
        set((s) => ({
          byBook: {
            ...s.byBook,
            [item.bookUrl]: [entry, ...(s.byBook[item.bookUrl] ?? [])],
          },
        }));
        return entry;
      },
      removeNote: (bookUrl, id) =>
        set((s) => ({
          byBook: {
            ...s.byBook,
            [bookUrl]: (s.byBook[bookUrl] ?? []).filter((n) => n.id !== id),
          },
        })),
      clearBook: (bookUrl) =>
        set((s) => {
          const next = { ...s.byBook };
          delete next[bookUrl];
          return { byBook: next };
        }),
    }),
    {
      name: "mojuan.notes.v2",
      version: 1,
      storage: createJSONStorage(() => localStorage),
    },
  ),
);
