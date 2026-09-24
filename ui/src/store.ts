import { create } from "zustand";
import type { Entity, Region, TaskView } from "./api";
interface Workbench {
  task: TaskView | null;
  undo: { entities: Entity[]; regions: Region[] }[];
  generation: number;
  saving: number;
  saveError: string;
  dirty: boolean;
  batchId: string | null;
  pendingTaskId: string | null;
  setTask: (task: TaskView | null) => void;
  setBatchId: (id: string | null) => void;
  markSaved: (task?: TaskView, generation?: number) => void;
  change: (entities: Entity[]) => void;
  replaceRegions: (regions: Region[], remember?: boolean) => void;
  setRevision: (revision: number) => void;
  undoLast: () => void;
}
// Sensitive task contents deliberately never use persist/localStorage.
export const useWorkbench = create<Workbench>((set) => ({
  task: null,
  undo: [],
  generation: 0,
  saving: 0,
  saveError: "",
  dirty: false,
  batchId: null,
  pendingTaskId: null,
  setTask: (task) =>
    set((s) => ({
      task,
      undo: [],
      dirty: false,
      generation: s.generation + 1,
      saveError: "",
    })),
  setBatchId: (batchId) => set({ batchId }),
  markSaved: (task, generation) =>
    set((state) => {
      if (task && state.task?.meta.id !== task.meta.id) return {};
      if (generation !== undefined && generation !== state.generation)
        return task && state.task
          ? {
              task: { ...state.task, revision: task.revision, meta: task.meta },
            }
          : {};
      return { task: task ?? state.task, dirty: false };
    }),
  change: (entities) =>
    set((s) =>
      s.task
        ? {
            undo: [
              ...s.undo.slice(-19),
              { entities: s.task.entities, regions: s.task.regions },
            ],
            task: { ...s.task, entities, preview: null },
            dirty: true,
            generation: s.generation + 1,
          }
        : {},
    ),
  replaceRegions: (regions, remember = true) =>
    set((s) =>
      s.task
        ? {
            task: { ...s.task, regions },
            generation: s.generation + 1,
            undo: remember
              ? [
                  ...s.undo.slice(-19),
                  { entities: s.task.entities, regions: s.task.regions },
                ]
              : s.undo,
          }
        : {},
    ),
  setRevision: (revision) =>
    set((state) => (state.task ? { task: { ...state.task, revision } } : {})),
  undoLast: () =>
    set((s) =>
      s.task && s.undo.length
        ? {
            task: {
              ...s.task,
              entities: s.undo[s.undo.length - 1].entities,
              regions: s.undo[s.undo.length - 1].regions,
              preview: null,
            },
            undo: s.undo.slice(0, -1),
            dirty: true,
            generation: s.generation + 1,
          }
        : {},
    ),
}));
