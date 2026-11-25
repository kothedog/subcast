// Chinese translations of titles and show notes, fetched lazily as they appear on screen. Requests made within
// a short window are sent together; results are cached in memory here and on disk by the Rust side.
// Only Chinese interfaces get them, in the script the interface uses (zh-CN or zh-TW).
import { useEffect, useMemo } from "react";
import { create } from "zustand";
import { useShallow } from "zustand/react/shallow";
import { api } from "./api";
import { t, useLang, type Lang } from "./i18n";

const CJK = /[㐀-鿿]/;
/** True when the text is mostly Chinese already (no translation needed). */
export const isZh = (text: string) => {
  const cjk = (text.match(/[㐀-鿿]/g) ?? []).length;
  return cjk > 0 && cjk >= text.replace(/\s/g, "").length * 0.25;
};

type Target = "zh-CN" | "zh-TW";
const targetOf = (l: Lang): Target | null => (l === "zh-TW" ? "zh-TW" : l === "zh-CN" ? "zh-CN" : null);
/** Cache and queue entries are keyed per target script: `${target}|${text}`. */
const keyOf = (target: Target, text: string) => `${target}|${text}`;

const useZhStore = create<{ map: Record<string, string> }>(() => ({ map: {} }));
const queued = new Set<string>();
const inFlight = new Set<string>();
let timer: number | undefined;

async function flush() {
  timer = undefined;
  const keys = [...queued].slice(0, 60);
  keys.forEach((k) => { queued.delete(k); inFlight.add(k); });
  if (queued.size) timer = window.setTimeout(flush, 60);
  const byTarget = new Map<Target, string[]>();
  for (const k of keys) {
    const i = k.indexOf("|");
    const target = k.slice(0, i) as Target;
    byTarget.set(target, [...(byTarget.get(target) ?? []), k.slice(i + 1)]);
  }
  await Promise.all([...byTarget].map(async ([target, texts]) => {
    try {
      const out = await api.translateTexts(texts, target);
      useZhStore.setState((s) => {
        const map = { ...s.map };
        texts.forEach((x, i) => { if (out[i]) map[keyOf(target, x)] = out[i]; });
        return { map };
      });
    } catch {
      /* offline: titles simply stay in the original language */
    } finally {
      texts.forEach((x) => inFlight.delete(keyOf(target, x)));
    }
  }));
}

function request(key: string) {
  if (useZhStore.getState().map[key] || queued.has(key) || inFlight.has(key)) return;
  queued.add(key);
  if (!timer) timer = window.setTimeout(flush, 50);
}

/** The Chinese version of `text`, or undefined while loading / when the text is already Chinese / in a non-Chinese UI. */
export function useZh(text: string | undefined | null): string | undefined {
  const target = targetOf(useLang((s) => s.lang));
  const clean = (text ?? "").trim();
  const key = target && clean.length > 0 && !isZh(clean) ? keyOf(target, clean) : null;
  const value = useZhStore((s) => (key ? s.map[key] : undefined));
  useEffect(() => {
    if (key && !value) request(key);
  }, [key, value]);
  return value;
}

/** Looks up the Chinese already cached for a text, without requesting any. Used to match a search typed in
 *  Chinese against names the lists show translated. */
export function useZhLookup(): (text: string) => string | undefined {
  const target = targetOf(useLang((s) => s.lang));
  const map = useZhStore((s) => s.map);
  return (text) => (target ? map[keyOf(target, text.trim())] : undefined);
}

/** Matches a typed filter against a show's name: the original, its curated Chinese name and any cached
 *  translation, ignoring case. An empty filter matches everything. */
export function useNameMatch(): (q: string, ...texts: (string | undefined)[]) => boolean {
  const zh = useZhLookup();
  return (q, ...texts) => {
    const needle = q.trim().toLowerCase();
    if (!needle) return true;
    return texts.some((x) => {
      if (!x) return false;
      const short = shortName(x);
      return [x, short, showName(x), zh(short)].some((v) => v?.toLowerCase().includes(needle));
    });
  };
}

/** Long text (show notes): split into paragraphs so each is translated and cached on its own. */
export function useZhParagraphs(text: string | undefined, enabled: boolean): string[] | undefined {
  const target = targetOf(useLang((s) => s.lang));
  const paras = useMemo(
    () =>
      target && enabled && text && !isZh(text)
        ? text.split(/\n{2,}/).map((p) => p.trim()).filter(Boolean).slice(0, 40).map((p) => p.slice(0, 3000))
        : [],
    [target, enabled, text],
  );
  const keys = useMemo(() => (target ? paras.map((p) => keyOf(target, p)) : []), [target, paras]);
  const vals = useZhStore(useShallow((s) => keys.map((k) => s.map[k])));
  useEffect(() => {
    for (let i = 0; i < keys.length; i++) {
      if (!vals[i]) request(keys[i]);
    }
  }, [keys, vals]);
  if (!keys.length || vals.some((v) => !v)) return undefined;
  return vals as string[];
}

export { CJK };

import { PICKS, shortName, showName } from "./picks";
import { useStore } from "../store";

/** A pure transliteration of a person's name ("德瓦克什·帕特尔") tells a Chinese reader nothing; drop it. */
const isTransliteration = (zh: string) => /^[\u4e00-\u9fff]{1,8}(·[\u4e00-\u9fff]{1,8})+$/.test(zh.trim());

/** Bilingual show name: user's custom alias first, else a curated Chinese name or machine translation when
 *  `translateShowNames` is enabled, plus the original. */
export function useShowName(title: string, showId?: string): { zh?: string; orig: string; aliased?: boolean } {
  const alias = useStore((s) => (showId ? s.settings.showAliases?.[showId]?.trim() : undefined));
  const chinese = targetOf(useLang((s) => s.lang)) != null;
  const names = useLang((s) => s.names);
  const on = chinese && names && !alias;
  const orig = shortName(title);
  const pick = PICKS.find((x) => x.title === title || shortName(x.title) === orig);
  // Not requested at all when off or aliased: no translation work for names the user does not want translated.
  const auto = useZh(on && !pick ? orig : undefined);
  if (alias) {
    return { zh: alias !== orig ? alias : undefined, orig: alias === orig ? alias : orig, aliased: true };
  }
  const zh = !on ? undefined : pick ? t(pick.zh) : auto && !isTransliteration(auto) ? auto : undefined;
  return { zh: zh && zh !== orig && !isZh(orig) ? zh : undefined, orig };
}
