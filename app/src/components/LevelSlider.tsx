import { useEffect, useRef, useState, type KeyboardEvent } from "react";

/**
 * A slider over a few levels: the handle follows the pointer smoothly and,
 * when let go, glides to the nearest level and settles there. Arrow keys,
 * Home and End move level by level.
 */
export function LevelSlider({
  levels,
  value,
  label,
  valueText,
  onChange,
  onPreview,
}: {
  levels: number;
  /** The saved level (0 … levels-1). */
  value: number;
  label: string;
  /** Spoken and shown name of a level. */
  valueText: (level: number) => string;
  /** Called once with the level the handle settled on. */
  onChange: (level: number) => void;
  /** The level nearest to the handle while it moves (null: at rest again). */
  onPreview?: (level: number | null) => void;
}) {
  const max = levels - 1;
  const [pos, setPos] = useState(value);
  const [dragging, setDragging] = useState(false);
  const animating = useRef<number | null>(null);
  const moved = useRef(false);

  // The saved level, unless the user is moving the handle right now.
  useEffect(() => {
    if (!dragging && animating.current === null) setPos(value);
  }, [value, dragging]);
  useEffect(() => () => cancelAnimationFrame(animating.current ?? 0), []);

  const settle = (from: number, to: number) => {
    cancelAnimationFrame(animating.current ?? 0);
    const start = performance.now();
    // Without motion when the system asks for less of it.
    const duration = window.matchMedia?.("(prefers-reduced-motion: reduce)").matches ? 0 : 160;
    const step = (now: number) => {
      const k = Math.min(1, (now - start) / duration);
      const eased = 1 - (1 - k) ** 3;
      setPos(from + (to - from) * eased);
      if (k < 1) animating.current = requestAnimationFrame(step);
      else {
        animating.current = null;
        setPos(to);
        if (to !== value) onChange(to);
        else onPreview?.(null);
      }
    };
    animating.current = requestAnimationFrame(step);
  };
  const release = () => {
    if (!dragging) return;
    setDragging(false);
    if (moved.current) settle(pos, Math.round(pos));
    moved.current = false;
  };
  const onKey = (e: KeyboardEvent<HTMLInputElement>) => {
    const at = Math.round(pos);
    const to =
      e.key === "ArrowLeft" || e.key === "ArrowDown"
        ? Math.max(0, at - 1)
        : e.key === "ArrowRight" || e.key === "ArrowUp"
          ? Math.min(max, at + 1)
          : e.key === "Home"
            ? 0
            : e.key === "End"
              ? max
              : null;
    if (to === null) return;
    e.preventDefault();
    settle(pos, to);
  };
  const nearest = Math.round(pos);
  useEffect(() => {
    if (dragging || animating.current !== null) onPreview?.(nearest);
    // Only when the nearest level changes.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [nearest]);
  return (
    <input
      type="range"
      className={dragging ? "level-slider dragging" : "level-slider"}
      min={0}
      max={max}
      step="any"
      value={pos}
      aria-label={label}
      aria-valuetext={valueText(nearest)}
      style={{ ["--fill" as string]: `${(pos / max) * 100}%` }}
      onPointerDown={() => {
        cancelAnimationFrame(animating.current ?? 0);
        animating.current = null;
        setDragging(true);
        moved.current = false;
      }}
      onChange={(e) => {
        const v = Number(e.target.value);
        if (dragging) {
          moved.current = true;
          setPos(v);
        } else {
          // A click on the track (or assistive technology) jumps to the nearest level.
          settle(pos, Math.round(v));
        }
      }}
      onPointerUp={release}
      onPointerCancel={release}
      onLostPointerCapture={release}
      onBlur={release}
      onKeyDown={onKey}
    />
  );
}
