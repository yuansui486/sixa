import { useEffect, useRef, useState } from "react";
import { LoaderCircle } from "lucide-react";
import { acquirePage } from "./preview-cache";
import { message, type DocumentPreview } from "./api";
export function PageImage({
  id,
  result,
  revision,
  page,
  onVisible,
  draft = false,
  enabled = true,
  maxDimension = 1400,
  label = `第 ${page.index + 1} 页`,
}: {
  id: string;
  result: boolean;
  revision: number;
  page: DocumentPreview["pages"][number];
  onVisible?: (visible: boolean) => void;
  draft?: boolean;
  enabled?: boolean;
  maxDimension?: number;
  label?: string;
}) {
  const ref = useRef<HTMLDivElement>(null);
  const callback = useRef(onVisible);
  callback.current = onVisible;
  const [visible, setVisible] = useState(false);
  const [image, setImage] = useState({ key: "", url: "", error: "" });
  const [retry, setRetry] = useState(0);
  const pixelRevision = result || draft ? revision : 0;
  const key = `${id}:${result}:${draft}:${pixelRevision}:${page.index}:${maxDimension}:${retry}`;
  useEffect(() => {
    const observer = new IntersectionObserver(
      ([entry]) => {
        setVisible(entry.isIntersecting);
        callback.current?.(entry.isIntersecting);
      },
      { rootMargin: "650px" },
    );
    if (ref.current) observer.observe(ref.current);
    return () => observer.disconnect();
  }, []);
  const direct = draft ? "" : page.preview_uri;
  useEffect(() => {
    if (!visible || !enabled || direct) return;
    const controller = new AbortController();
    let release: (() => void) | undefined;
    acquirePage(
      id,
      result,
      page.index,
      revision,
      page.width,
      page.height,
      maxDimension,
      { draft, signal: controller.signal },
    )
      .then((entry) => {
        if (controller.signal.aborted) entry.release();
        else {
          release = entry.release;
          setImage({ key, url: entry.url, error: "" });
        }
      })
      .catch((error) => {
        if (!controller.signal.aborted)
          setImage({ key, url: "", error: message(error) });
      });
    return () => {
      controller.abort();
      release?.();
      setImage((current) =>
        current.key === key ? { key: "", url: "", error: "" } : current,
      );
    };
  }, [key, visible, enabled, direct]);
  const current = image.key === key && enabled && visible;
  const url = direct || (current ? image.url : "");
  const error = current ? image.error : "";
  return (
    <div
      ref={ref}
      className="page-image"
      style={{ aspectRatio: `${page.width} / ${page.height}` }}
      aria-busy={visible && enabled && !url && !error}
    >
      {url ? (
        <img src={url} alt={label} />
      ) : (
        <div className="page-image-state" role="status">
          {error ? (
            <>
              <span>
                {draft ? "效果预览失败：" : "预览加载失败："}
                {error}
              </span>
              <button
                className="secondary"
                onClick={() => setRetry((value) => value + 1)}
              >
                重试此页
              </button>
            </>
          ) : visible && enabled ? (
            <>
              <LoaderCircle className="spin" size={18} />
              {draft ? `正在更新${label}的脱敏效果` : `正在加载${label}`}
            </>
          ) : (
            <span>{draft ? `${label} · 等待更新效果` : label}</span>
          )}
        </div>
      )}
    </div>
  );
}
