import { create } from "zustand";
import type { Entity, Region, TaskView } from "./api";
interface Workbench {
  task: TaskView | null;
  undo: Entity[][];
  dirty: boolean;
  batchId: string | null;
  setTask: (task: TaskView | null) => void;
  setBatchId: (id: string | null) => void;
  markSaved: (task?: TaskView) => void;
  change: (entities: Entity[]) => void;
  replaceRegions: (regions: Region[]) => void;
  setRevision: (revision: number) => void;
  undoLast: () => void;
}
// Sensitive task contents deliberately never use persist/localStorage.
export const useWorkbench = create<Workbench>((set) => ({
  task: null,
  undo: [],
  dirty: false,
  batchId: null,
  setTask: (task) => set({ task, undo: [], dirty: false }),
  setBatchId: (batchId) => set({ batchId }),
  markSaved: (task) =>
    set((state) => ({ task: task ?? state.task, dirty: false })),
  change: (entities) =>
    set((s) =>
      s.task
        ? {
            undo: [...s.undo.slice(-19), s.task.entities],
            task: { ...s.task, entities, preview: null },
            dirty: true,
          }
        : {},
    ),
  replaceRegions: (regions) =>
    set((state) => (state.task ? { task: { ...state.task, regions } } : {})),
  setRevision: (revision) =>
    set((state) => (state.task ? { task: { ...state.task, revision } } : {})),
  undoLast: () =>
    set((s) =>
      s.task && s.undo.length
        ? {
            task: {
              ...s.task,
              entities: s.undo[s.undo.length - 1],
              preview: null,
            },
            undo: s.undo.slice(0, -1),
            dirty: true,
          }
        : {},
    ),
}));
