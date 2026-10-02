import { useEffect, useRef, useState, type KeyboardEvent, type MouseEvent, type PointerEvent } from "react";

/** How far the pointer has to move before a press becomes a drag. */
const THRESHOLD = 5;

/** The order shown: the one being dragged, the one just saved (until the list catches up), or the list's own. */
function shown(ids: string[], held: { base: string; order: string[] } | null): string[] {
  if (!held || held.base !== ids.join("\n")) return ids;
  return held.order;
}

function moveTo(order: string[], id: string, index: number): string[] {
  const rest = order.filter((x) => x !== id);
  rest.splice(Math.max(0, Math.min(rest.length, index)), 0, id);
  return rest;
}

/**
 * Sorting a list by hand: press on an item and drag it (pointer events, so it
 * works the same in every web view), or Alt+↑/↓ on the focused item. The list
 * follows the pointer at once; the new order is saved once, on release.
 */
export function useReorder(ids: string[], save: (order: string[]) => Promise<unknown>) {
  const [held, setHeld] = useState<{ base: string; order: string[] } | null>(null);
  const [dragging, setDragging] = useState<string | null>(null);
  const rows = useRef(new Map<string, HTMLElement>());
  const press = useRef<{ id: string; y: number; active: boolean } | null>(null);
  const swallowClick = useRef(false);
  const order = shown(ids, held);
  const latest = useRef({ ids, order, save });
  latest.current = { ids, order, save };

  useEffect(() => {
    const move = (e: globalThis.PointerEvent) => {
      const p = press.current;
      if (!p) return;
      if (!p.active) {
        if (Math.abs(e.clientY - p.y) < THRESHOLD) return;
        p.active = true;
        setDragging(p.id);
        document.body.classList.add("reordering");
      }
      const { ids, order } = latest.current;
      // The new place: after every other item whose middle is above the pointer.
      let index = 0;
      for (const id of order) {
        if (id === p.id) continue;
        const r = rows.current.get(id)?.getBoundingClientRect();
        if (r && e.clientY > r.top + r.height / 2) index++;
      }
      const next = moveTo(order, p.id, index);
      if (next.join("\n") !== order.join("\n")) setHeld({ base: ids.join("\n"), order: next });
    };
    const up = () => {
      const p = press.current;
      press.current = null;
      if (!p?.active) return;
      setDragging(null);
      document.body.classList.remove("reordering");
      // The click that ends a drag must not also open the item.
      swallowClick.current = true;
      window.setTimeout(() => (swallowClick.current = false), 0);
      const { ids, order, save } = latest.current;
      if (order.join("\n") !== ids.join("\n")) void save(order).catch(() => setHeld(null));
    };
    window.addEventListener("pointermove", move);
    window.addEventListener("pointerup", up);
    window.addEventListener("pointercancel", up);
    return () => {
      window.removeEventListener("pointermove", move);
      window.removeEventListener("pointerup", up);
      window.removeEventListener("pointercancel", up);
      document.body.classList.remove("reordering");
    };
  }, []);

  const keyMove = (id: string, by: number) => {
    const at = order.indexOf(id);
    const next = moveTo(order, id, at + by);
    if (next.join("\n") === order.join("\n")) return;
    setHeld({ base: ids.join("\n"), order: next });
    void save(next).catch(() => setHeld(null));
  };

  return {
    order,
    dragging,
    /** Spread on the element that moves (the list item). */
    item: (id: string) => ({
      ref: (el: HTMLElement | null) => {
        if (el) rows.current.set(id, el);
        else rows.current.delete(id);
      },
      onPointerDown: (e: PointerEvent) => {
        // Only the primary button, and not from a text field or the item's own buttons.
        if (e.button !== 0 || (e.target as HTMLElement).closest("input, textarea, select, .item-actions")) return;
        e.stopPropagation();
        press.current = { id, y: e.clientY, active: false };
      },
      onClickCapture: (e: MouseEvent) => {
        if (!swallowClick.current) return;
        swallowClick.current = false;
        e.preventDefault();
        e.stopPropagation();
      },
    }),
    /** Spread on the item's main button: Alt+↑/↓ moves it. */
    keys: {
      "aria-keyshortcuts": "Alt+ArrowUp Alt+ArrowDown",
    },
    onKey: (id: string) => (e: KeyboardEvent) => {
      if (!e.altKey || (e.key !== "ArrowUp" && e.key !== "ArrowDown")) return;
      e.preventDefault();
      keyMove(id, e.key === "ArrowUp" ? -1 : 1);
      // Keep the focus on the moved item.
      const el = e.currentTarget as HTMLElement;
      window.requestAnimationFrame(() => el.focus());
    },
  };
}
