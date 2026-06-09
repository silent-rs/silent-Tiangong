import { create } from 'zustand';

export type SearchScope = 'messages' | 'withThinking' | 'all';

interface SearchState {
  searchActive: boolean;
  searchQuery: string;
  currentMatchIndex: number;
  searchScope: SearchScope;
  caseSensitive: boolean;
  currentMessageId: string | null;

  openSearch: () => void;
  closeSearch: () => void;
  setSearchQuery: (query: string) => void;
  setSearchScope: (scope: SearchScope) => void;
  setCaseSensitive: (v: boolean) => void;
  setCurrentMatchMessageId: (id: string | null) => void;
  nextMatch: (total: number) => void;
  prevMatch: (total: number) => void;
  setCurrentMatchIndex: (index: number) => void;
}

export const useSearchStore = create<SearchState>((set) => ({
  searchActive: false,
  searchQuery: '',
  currentMatchIndex: 0,
  searchScope: 'messages',
  caseSensitive: false,
  currentMessageId: null,

  openSearch: () => set({ searchActive: true }),

  closeSearch: () => set({ searchActive: false, searchQuery: '', currentMatchIndex: 0, currentMessageId: null }),

  setSearchQuery: (query) => set({ searchQuery: query, currentMatchIndex: 0 }),

  setSearchScope: (scope) => set({ searchScope: scope, currentMatchIndex: 0 }),

  setCaseSensitive: (v) => set({ caseSensitive: v, currentMatchIndex: 0 }),

  setCurrentMatchMessageId: (id) => set({ currentMessageId: id }),

  nextMatch: (total) =>
    set((state) => ({
      currentMatchIndex: total > 0 ? (state.currentMatchIndex + 1) % total : 0,
    })),

  prevMatch: (total) =>
    set((state) => ({
      currentMatchIndex: total > 0 ? (state.currentMatchIndex - 1 + total) % total : 0,
    })),

  setCurrentMatchIndex: (index) => set({ currentMatchIndex: index }),
}));
