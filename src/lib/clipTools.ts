// Highlights and clip-making: shown unless turned off in Settings › 通用 › 界面.
import { useStore } from "../store";

export function useClipTools(): boolean {
  return useStore((s) => s.settings.clipTools) ?? true;
}
