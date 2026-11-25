// Keeps a playing video alive across pages. The YouTube player lives once, at the app's root, and is only moved
// with CSS: over the episode page's video slot when that page is open, or into a small floating window when
// the listener goes elsewhere. Moving a node holding an iframe would reload it; this never moves it.
import { useEffect, useRef } from "react";
import { create } from "zustand";
import { useShallow } from "zustand/react/shallow";
import { ExternalLink, X } from "lucide-react";
import type { Episode } from "../lib/api";
import { YouTubePlayer } from "./Players";
import { useLayout } from "../lib/layout";
import { useStore } from "../store";
import { t } from "../lib/i18n";

interface Slot { ep: Episode; el: HTMLElement; overlay?: React.ReactNode; onUnplayable?: () => void }
interface Dock { slot: Slot | null; /** The episode playing in the floating window after its page closed. */ floating: { ep: Episode; overlay?: React.ReactNode } | null }
export const useVideoDock = create<Dock>(() => ({ slot: null, floating: null }));

/** The episode page's place for its video. Leaving the page while it plays keeps it playing, floating. */
export function VideoSlot({ ep, overlay, onUnplayable }: Omit<Slot, "el">) {
  const ref = useRef<HTMLDivElement>(null);
  useEffect(() => {
    const el = ref.current!;
    useVideoDock.setState({ slot: { ep, el, overlay, onUnplayable }, floating: null });
    return () => {
      const s = useStore.getState();
      const playing = s.current?.id === ep.id && s.playing;
      useVideoDock.setState((d) => (d.slot?.el === el ? { slot: null, floating: playing ? { ep, overlay } : null } : d));
    };
  }, [ep.id]);
  useEffect(() => { useVideoDock.setState((d) => (d.slot?.el === ref.current ? { slot: { ...d.slot!, overlay, onUnplayable } } : d)); }, [overlay, onUnplayable]);
  return <div className="video-slot" ref={ref} />;
}

export function VideoDock() {
  const { slot, floating } = useVideoDock(useShallow((d) => ({ slot: d.slot, floating: d.floating })));
  const box = useRef<HTMLDivElement>(null);
  const ep = slot?.ep ?? floating?.ep;
  const theater = useStore((s) => s.theater);

  // Sync dock geometry only when slot layout, scroll position, or pane widths change (no idle 60fps loop).
  // Keep left/top/width/height rather than transform: a parent transform would break fixed `.video.theater`.
  useEffect(() => {
    const b = box.current;
    if (!b) return;
    if (!slot) { b.removeAttribute("style"); return; }
    const scroller = slot.el.closest(".ep-left") as HTMLElement | null;
    let raf = 0, last = "";
    const sync = () => {
      raf = 0;
      const r = slot.el.getBoundingClientRect();
      const c = (scroller ?? document.body).getBoundingClientRect();
      const top = Math.max(0, c.top - r.top), bottom = Math.max(0, r.bottom - c.bottom);
      const key = `${r.left},${r.top},${r.width},${r.height},${top},${bottom}`;
      if (key === last) return;
      last = key;
      Object.assign(b.style, {
        left: `${r.left}px`,
        top: `${r.top}px`,
        width: `${r.width}px`,
        height: `${r.height}px`,
        clipPath: top || bottom ? `inset(${top}px 0 ${bottom}px 0 round 12px)` : "",
      });
    };
    const schedule = () => { if (!raf) raf = requestAnimationFrame(sync); };
    sync();
    const ro = new ResizeObserver(schedule);
    ro.observe(slot.el);
    ro.observe(document.documentElement);
    if (scroller) {
      ro.observe(scroller);
      for (const child of scroller.children) ro.observe(child);
    }
    const mo = scroller ? new MutationObserver((muts) => {
      for (const m of muts) {
        for (const n of m.addedNodes) if (n instanceof Element) ro.observe(n);
        for (const n of m.removedNodes) if (n instanceof Element) ro.unobserve(n);
      }
      schedule();
    }) : null;
    mo?.observe(scroller!, { childList: true });
    scroller?.addEventListener("scroll", schedule, { passive: true });
    window.addEventListener("resize", schedule, { passive: true });
    const unsubLayout = useLayout.subscribe(schedule);
    return () => {
      if (raf) cancelAnimationFrame(raf);
      ro.disconnect();
      mo?.disconnect();
      scroller?.removeEventListener("scroll", schedule);
      window.removeEventListener("resize", schedule);
      unsubLayout();
    };
  }, [slot]);

  // A floating video that is paused for good (ended, or another episode took over) goes away.
  const current = useStore((s) => s.current);
  useEffect(() => { if (floating && current?.id !== floating.ep.id) useVideoDock.setState({ floating: null }); }, [current?.id]);

  if (!ep) return null;
  const close = () => {
    const s = useStore.getState();
    if (s.playing) s.player?.toggle();
    useVideoDock.setState({ floating: null });
  };
  return (
    <div ref={box} className={`video-dock ${slot ? "docked" : "floating"} ${theater ? "theater-host" : ""}`}>
      <YouTubePlayer key={ep.id} ep={ep} overlay={slot?.overlay ?? floating?.overlay} onUnplayable={slot?.onUnplayable} />
      {!slot && (
        <div className="video-float-bar">
          <button className="icon-btn sm" title={t("回到这一集")} aria-label={t("回到这一集")} onClick={() => useStore.getState().openEpisode(ep)}><ExternalLink size={14} /></button>
          <button className="icon-btn sm" title={t("停止播放并关闭")} aria-label={t("停止播放并关闭")} onClick={close}><X size={14} /></button>
        </div>
      )}
    </div>
  );
}
