import { invoke } from "@tauri-apps/api/core";
export type TaskState =
  | "queued"
  | "analyzing"
  | "awaiting_review"
  | "processing"
  | "completed"
  | "partial"
  | "failed"
  | "cancelled";
export interface TaskMeta {
  id: string;
  kind: string;
  state: TaskState;
  created_at: number;
  updated_at: number;
  error: string | null;
  error_info?: ErrorInfo | null;
  display_name?: string;
  file_size?: number;
  parent_batch_id?: string | null;
  reviewed_revision?: number;
  storage_bytes?: number;
}
export interface ErrorInfo {
  code: string;
  title: string;
  detail: string;
  recovery_action: string;
  retryable: boolean;
  diagnostic_id: string;
}
export interface Entity {
  id: string;
  entity_type: string;
  type_label: string;
  score: number;
  source: string;
  selected: boolean;
  display: { start: number; end: number };
  text: string;
  replacement: string | null;
  effective_replacement?: string;
}
export interface Selection {
  id: string;
  selected: boolean;
  replacement: string | null;
}
export interface TaskView {
  meta: TaskMeta;
  text: string;
  entities: Entity[];
  preview: string | null;
  extension: string;
  regions: Region[];
  options: TaskOptions;
  warnings: string[];
  revision: number;
}
export type OcrProfile = "mobile" | "accurate";
export type PdfMode = "safe_rebuild" | "fidelity";
export interface TaskOptions {
  ocr_profile: OcrProfile;
  pdf_mode: PdfMode;
}
export interface Point {
  x: number;
  y: number;
}
export interface Region {
  id: string;
  page: number;
  polygon: Point[];
  entity_id: string | null;
  selected: boolean;
  source: "ocr" | "entity" | "manual" | "embedded_image";
  text: string;
  score: number | null;
  rotation: number;
  replacement: string | null;
}
export interface DocumentPreview {
  kind?: "pages" | "docx" | "office_content" | "text";
  revision: number;
  pages: {
    index: number;
    width: number;
    height: number;
    preview_uri: string;
  }[];
  text: string | null;
  warnings: string[];
}
export interface OfficeAnchor {
  id: string;
  text: string;
  display: { start: number; end: number };
  label: string;
  available_in_layout: boolean;
}
export interface OfficeImage {
  index: number;
  name: string;
  occurrences: string[];
}
export interface OfficePreview {
  revision: number;
  layout_available: boolean;
  reason: string | null;
  anchors: OfficeAnchor[];
  images: OfficeImage[];
  warnings: string[];
}
export interface RegionMutationAck {
  revision: number;
  region_id: string;
}
export interface AppSettings extends TaskOptions {
  concurrency: number;
}
export type CloseBehavior = "ask" | "tray" | "exit";
export interface DesktopPreferences {
  close_behavior: CloseBehavior;
  tray_available: boolean;
}
export interface CloseRequest {
  id: string;
  phase: "choice" | "saving" | "confirm" | "stopping" | "slow";
  tray_available: boolean;
  active_tasks: number;
  active_downloads: number;
}
export type CloseAction =
  "tray" | "exit" | "saved" | "stop" | "cancel" | "force" | "wait";
export interface ModelPackageStatus {
  id: string;
  version: string;
  profile: string;
  size: number;
  location: string;
  installed: boolean;
  ready: boolean;
  error: string | null;
  state?:
    "missing" | "downloading" | "installed" | "loading" | "ready" | "failed";
}
export interface ModelStatus {
  ready: boolean;
  version: string | null;
  location: string;
  bytes: number;
  error: string | null;
  capabilities?: ModelCapability[];
}
export interface ModelCapability {
  id: string;
  label: string;
  installed: boolean;
  ready: boolean;
  version: string | null;
  bytes: number;
  location: string;
  error: string | null;
  state?: ModelPackageStatus["state"];
}
export interface Rule {
  id: string;
  name: string;
  entity_type: string;
  kind: "literal" | "regex" | "dictionary";
  pattern: string;
  enabled: boolean;
}
export interface Policy {
  entity_type: string;
  replacement: string;
}
export interface BuiltinPolicy {
  entity_type: string;
  type_label: string;
  behavior: string;
  examples: { original: string; replacement: string }[];
}
export interface Bootstrap {
  tasks: TaskMeta[];
  model: ModelStatus;
  data_dir: string;
}
export interface AuthSubject {
  id: string;
  username: string;
  display_name: string | null;
  tenant_id: string;
  tenant_code: string;
  tenant_name: string;
}
export interface ProductPolicy {
  product_code: string;
  module_enabled: boolean;
  concurrent_device_limit: number;
  active_session_count: number;
  heartbeat_interval_seconds: number;
  offline_grace_seconds: number;
}
export interface AuthStatus {
  authenticated: boolean;
  offline: boolean;
  offline_until: number | null;
  subject: AuthSubject | null;
  policy: ProductPolicy | null;
  reason: string | null;
}
export interface IntegrationInfo {
  enabled: boolean;
  mcp_available: boolean;
  authenticated: boolean;
  models_ready: boolean;
  executable_path: string;
  protocol_version: string;
  supported_formats: string[];
}
export interface IntegrationCheck {
  ok: boolean;
  message: string;
}
export interface BatchView {
  meta: TaskMeta;
  source_batch_id?: string | null;
  items: {
    index: number;
    task_id: string | null;
    state: TaskState;
    error: string | null;
    error_info?: ErrorInfo | null;
    display_name?: string;
    extension?: string;
    file_size?: number;
    reviewed_revision?: number;
    review_confirmed?: boolean;
    revision?: number;
  }[];
}
interface Commands {
  get_desktop_preferences: { args: undefined; result: DesktopPreferences };
  set_desktop_preferences: {
    args: { closeBehavior: CloseBehavior };
    result: DesktopPreferences;
  };
  get_close_request: { args: undefined; result: CloseRequest | null };
  acknowledge_app_close: { args: { requestId: string }; result: void };
  respond_app_close: {
    args: { requestId: string; action: CloseAction; remember?: boolean };
    result: CloseRequest | null;
  };
  review_patch: {
    args: {
      id: string;
      selections: Selection[];
      expectedRevision: number;
      mutationId: string;
    };
    result: TaskView;
  };
  confirm_review: {
    args: { id: string; expectedRevision: number };
    result: TaskView;
  };
  clone_for_review: { args: { id: string }; result: TaskView };
  retry_task: { args: { id: string }; result: TaskView };
  document_manifest: {
    args: { id: string; result: boolean };
    result: DocumentPreview;
  };
  document_page: {
    args: {
      id: string;
      result: boolean;
      page: number;
      maxDimension: number;
      expectedRevision: number;
    };
    result: ArrayBuffer | number[];
  };
  document_draft_page: {
    args: {
      id: string;
      page: number;
      maxDimension: number;
      expectedRevision: number;
      requestId: string;
    };
    result: ArrayBuffer | number[];
  };
  cancel_document_preview: { args: { requestId: string }; result: void };
  office_preview: {
    args: { id: string; result: boolean; expectedRevision: number };
    result: OfficePreview;
  };
  office_preview_docx: {
    args: { id: string; result: boolean; expectedRevision: number };
    result: ArrayBuffer | number[];
  };
  query_tasks: {
    args: {
      query: {
        search?: string;
        state?: TaskState;
        from?: number;
        to?: number;
        offset: number;
        limit: number;
      };
    };
    result: {
      items: TaskMeta[];
      total: number;
      offset?: number;
      limit?: number;
    };
  };
  retry_batch: {
    args: { id: string; failedOnly: boolean; requestId?: string };
    result: BatchView;
  };
  reveal_file: { args: { path: string }; result: void };
  test_rule: { args: { rule: Rule; text: string }; result: Entity[] };
  retry_model_load: { args: { packageId: string }; result: ModelStatus };
  auth_status: { args: undefined; result: AuthStatus };
  auth_login: {
    args: { tenantCode: string; username: string; password: string };
    result: AuthStatus;
  };
  auth_logout: { args: undefined; result: void };
  initialize: { args: undefined; result: Bootstrap };
  analyze_file: {
    args: { path: string; options?: TaskOptions; requestId?: string };
    result: TaskView;
  };
  task_view: { args: { id: string }; result: TaskView };
  select_entities: {
    args: { id: string; selections: Selection[] };
    result: TaskView;
  };
  preview: { args: { id: string }; result: string };
  document_preview: { args: { id: string }; result: DocumentPreview };
  document_result_preview: { args: { id: string }; result: DocumentPreview };
  upsert_region: {
    args: {
      id: string;
      region: Region;
      expectedRevision: number;
      mutationId?: string;
    };
    result: RegionMutationAck;
  };
  remove_region: {
    args: {
      id: string;
      regionId: string;
      expectedRevision: number;
      mutationId?: string;
    };
    result: RegionMutationAck;
  };
  execute: { args: { id: string }; result: TaskView };
  export_task: { args: { id: string; path: string }; result: void };
  export_recovery: {
    args: { id: string; path: string; password: string };
    result: void;
  };
  restore: {
    args: { source: string; destination: string; password: string };
    result: void;
  };
  list_tasks: { args: undefined; result: TaskMeta[] };
  delete_task: { args: { id: string }; result: void };
  cancel_task: { args: { id: string }; result: void };
  list_rules: { args: undefined; result: Rule[] };
  save_rule: { args: { rule: Rule }; result: void };
  delete_rule: { args: { id: string }; result: void };
  list_policies: { args: undefined; result: Policy[] };
  builtin_policies: { args: undefined; result: BuiltinPolicy[] };
  save_policy: { args: { policy: Policy }; result: void };
  model_status: { args: undefined; result: ModelStatus };
  load_models: { args: undefined; result: ModelStatus };
  ensure_default_models: { args: undefined; result: ModelStatus };
  model_packages: { args: undefined; result: ModelPackageStatus[] };
  install_model: {
    args: { packageId: string };
    result: ModelStatus;
  };
  rebuild_model: {
    args: { packageId: string };
    result: ModelStatus;
  };
  cancel_model_install: { args: { packageId: string }; result: void };
  get_settings: { args: undefined; result: AppSettings };
  save_settings: { args: { settings: AppSettings }; result: AppSettings };
  integration_info: { args: undefined; result: IntegrationInfo };
  integration_check: { args: undefined; result: IntegrationCheck };
  create_batch: {
    args: { paths: string[]; requestId?: string };
    result: BatchView;
  };
  batch_view: { args: { id: string }; result: BatchView };
  execute_batch: { args: { id: string }; result: BatchView };
  export_batch: { args: { id: string; path: string }; result: void };
}
export function call<K extends keyof Commands>(
  command: K,
  ...args: Commands[K]["args"] extends undefined ? [] : [Commands[K]["args"]]
): Promise<Commands[K]["result"]> {
  return invoke(command, args[0] as Record<string, unknown> | undefined);
}
export function message(error: unknown): string {
  if (typeof error === "string") return error;
  if (error && typeof error === "object") {
    if ("title" in error && "detail" in error)
      return `${String(error.title)}：${String(error.detail)}`;
    if ("message" in error) return String(error.message);
  }
  return "操作失败，请重试";
}
export function formatBytes(bytes = 0): string {
  if (!Number.isFinite(bytes) || bytes <= 0) return "0 B";
  const units = ["B", "KB", "MB", "GB"];
  const index = Math.min(
    Math.floor(Math.log(bytes) / Math.log(1024)),
    units.length - 1,
  );
  const value = bytes / 1024 ** index;
  return `${value.toFixed(index === 0 || value >= 10 ? 0 : 1)} ${units[index]}`;
}
export function capabilityReady(
  model: ModelStatus | undefined,
  id: string,
): boolean {
  const capability = model?.capabilities?.find((item) => item.id === id);
  return capability
    ? capability.ready
    : id === "raner-v1"
      ? !!model?.ready
      : false;
}
export function capabilityInstalled(
  model: ModelStatus | undefined,
  id: string,
): boolean {
  const capability = model?.capabilities?.find((item) => item.id === id);
  return capability
    ? capability.installed
    : id === "raner-v1"
      ? !!model?.ready
      : false;
}
export const stateLabels: Record<TaskState, string> = {
  queued: "排队中",
  analyzing: "识别中",
  awaiting_review: "等待复核",
  processing: "生成中",
  completed: "已完成",
  partial: "部分完成",
  failed: "失败",
  cancelled: "已取消",
};
export function selection(entities: Entity[]): Selection[] {
  return entities.map(({ id, selected, replacement }) => ({
    id,
    selected,
    replacement,
  }));
}
export function regionUpdate(
  id: string,
  region: Region,
): { id: string; region: Region };
export function regionUpdate(
  id: string,
  region: Region,
  expectedRevision: number,
): { id: string; region: Region; expectedRevision: number };
export function regionUpdate(
  id: string,
  region: Region,
  expectedRevision?: number,
) {
  return expectedRevision === undefined
    ? { id, region }
    : { id, region, expectedRevision };
}
