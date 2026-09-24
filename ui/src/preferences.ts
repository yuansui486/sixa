import { create } from "zustand";
import { persist } from "zustand/middleware";
export type SidebarMode = "auto" | "expanded" | "collapsed";
export const usePreferences = create(
  persist<{
    sidebar: SidebarMode;
    inspectorWidth: number;
    inspectorOpen: boolean;
    focusMode: boolean;
    compareRatio: number;
    lastDirectory: string;
  }>(
    () => ({
      sidebar: "auto",
      inspectorWidth: 320,
      inspectorOpen: true,
      focusMode: false,
      compareRatio: 50,
      lastDirectory: "",
    }),
    { name: "sixa-desktop-layout" },
  ),
);
export function exportName(displayName: string | undefined, extension: string) {
  const name = (displayName || "文件").replace(/\.[^.]+$/, "");
  const directory = usePreferences.getState().lastDirectory;
  return `${directory ? `${directory}${directory.includes("\\") ? "\\" : "/"}` : ""}${name}_脱敏.${extension}`;
}
export function rememberDirectory(path: string) {
  const end = Math.max(path.lastIndexOf("/"), path.lastIndexOf("\\"));
  if (end >= 0) usePreferences.setState({ lastDirectory: path.slice(0, end) });
}
