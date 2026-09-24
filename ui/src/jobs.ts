import { create } from "zustand";
export interface JobProgress {
  id: string;
  source_batch_id?: string;
  stage?: string;
  done?: number;
  total?: number;
  terminal?: boolean;
  batch?: boolean;
}
export const useJobs = create<{
  jobs: Record<string, JobProgress>;
  update: (job: JobProgress) => void;
}>((set) => ({
  jobs: {},
  update: (job) =>
    set((s) => ({
      jobs: { ...s.jobs, [job.id]: { ...s.jobs[job.id], ...job } },
    })),
}));
export function runningJob(job: JobProgress | undefined) {
  return (
    !!job &&
    !job.terminal &&
    ["queued", "analyzing", "processing", "validating", "exporting"].includes(
      job.stage ?? "",
    )
  );
}
