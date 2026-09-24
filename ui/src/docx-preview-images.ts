import { acquirePage } from "./preview-cache";
import { message, type DocumentPreview, type TaskView } from "./api";

export type ImageOriginals = Map<
  Element,
  { src: string | null; href: string | null; visibility: string }
>;
export function originalImages(body: HTMLElement): ImageOriginals {
  return new Map(
    [...body.querySelectorAll<HTMLElement>("[data-image-index]")].map(
      (image) => [
        image,
        {
          src: image.getAttribute("src"),
          href: image.getAttribute("href"),
          visibility: image.style.visibility,
        },
      ],
    ),
  );
}

/** Hide pending draft images immediately. Preserve original sizing, rotation and cropping. */
export function draftImages({
  body,
  originals,
  task,
  enabled,
  ready,
  revision,
  page,
  imagePages,
  retry,
}: {
  body: HTMLElement;
  originals: ImageOriginals;
  task: TaskView;
  enabled: boolean;
  ready: boolean;
  revision: number;
  page: number;
  imagePages?: DocumentPreview["pages"];
  retry: () => void;
}) {
  const controller = new AbortController();
  const leases: (() => void)[] = [];
  const labels: HTMLElement[] = [];
  const positions = new Map<HTMLElement, string>();
  const entities = new Map(task.entities.map((entity) => [entity.id, entity]));
  const selected = new Set(
    task.regions
      .filter((region) =>
        region.entity_id && entities.has(region.entity_id)
          ? entities.get(region.entity_id)!.selected
          : region.selected,
      )
      .map((region) => region.page),
  );
  const pages = [...body.querySelectorAll("section.sixa-word")];
  const grouped = new Map<
    number,
    { image: HTMLElement; label: HTMLElement }[]
  >();
  const restore = (image: Element) => {
    const original = originals.get(image)!;
    for (const [attribute, value] of [
      ["src", original.src],
      ["href", original.href],
    ] as const) {
      if (value === null) image.removeAttribute(attribute);
      else image.setAttribute(attribute, value);
    }
    (image as HTMLElement).style.visibility = original.visibility;
  };
  for (const [element] of originals) {
    restore(element);
    const image = element as HTMLElement;
    const index = Number(image.dataset.imageIndex);
    if (!enabled || !selected.has(index)) continue;
    image.style.visibility = "hidden";
    const label = document.createElement("span");
    label.className = "sixa-docx-image-state";
    label.setAttribute("role", "status");
    label.textContent = ready
      ? "图片效果将在当前页更新"
      : "等待保存并更新图片效果";
    const frame = image.closest("svg")?.parentElement ?? image.parentElement;
    if (frame) {
      if (!positions.has(frame)) positions.set(frame, frame.style.position);
      if (!frame.style.position || frame.style.position === "static")
        frame.style.position = "relative";
      frame.append(label);
      labels.push(label);
    }
    const section = image.closest("section.sixa-word");
    const visible = !pages.length || pages.indexOf(section!) === page;
    if (visible)
      grouped.set(index, [...(grouped.get(index) ?? []), { image, label }]);
  }
  if (enabled && ready)
    void (async () => {
      // One image resource at a time; shared occurrences use the same cached pixels.
      for (const [index, references] of grouped) {
        if (controller.signal.aborted) return;
        references.forEach(({ label }) => {
          label.textContent = "正在更新图片脱敏效果";
        });
        try {
          const dimensions = imagePages?.find((item) => item.index === index);
          const lease = await acquirePage(
            task.meta.id,
            false,
            index,
            revision,
            dimensions?.width ?? 1400,
            dimensions?.height ?? 1400,
            1400,
            { draft: true, signal: controller.signal },
          );
          if (controller.signal.aborted) {
            lease.release();
            return;
          }
          leases.push(lease.release);
          for (const { image, label } of references) {
            image.setAttribute(
              image.localName === "image" ? "href" : "src",
              lease.url,
            );
            image.style.visibility = originals.get(image)!.visibility;
            label.remove();
          }
        } catch (error) {
          if (controller.signal.aborted) return;
          for (const { label } of references) {
            label.textContent = "图片效果更新失败，点击重试";
            label.title = message(error);
            label.setAttribute("role", "button");
            label.tabIndex = 0;
            label.onclick = (event) => {
              event.stopPropagation();
              retry();
            };
            label.onkeydown = (event) => {
              if (["Enter", " "].includes(event.key)) {
                event.preventDefault();
                event.stopPropagation();
                retry();
              }
            };
          }
        }
      }
    })();
  return () => {
    controller.abort();
    leases.forEach((release) => release());
    labels.forEach((label) => label.remove());
    positions.forEach((position, frame) => {
      frame.style.position = position;
    });
    originals.forEach((_, image) => restore(image));
  };
}
