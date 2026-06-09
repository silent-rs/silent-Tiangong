import { create } from 'zustand';

interface SearchState {
  searchActive: boolean;
  searchQuery: string;
  currentMatchIndex: number;

  openSearch: () => void;
  closeSearch: () => void;
  setSearchQuery: (query: string) => void;
  nextMatch: (total: number) => void;
  prevMatch: (total: number) => void;
  setCurrentMatchIndex: (index: number) => void;
}

export const useSearchStore = create<SearchState>((set) => ({
  searchActive: false,
  searchQuery: '',
  currentMatchIndex: 0,

  openSearch: () => set({ searchActive: true }),

  closeSearch: () => set({ searchActive: false, searchQuery: '', currentMatchIndex: 0 }),

  setSearchQuery: (query) => set({ searchQuery: query, currentMatchIndex: 0 }),

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
