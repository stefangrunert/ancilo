import type { OpInput, OpOutput } from "../api/client";
import { useClient, useOp, useRefresh } from "./store";

export type Preferences = OpOutput<"get_preferences">;
export type Step = NonNullable<OpInput<"set_preferences">>["step"] & string;

/** The app's preferences (view, purposes, setup progress), kept in the daemon. */
export function usePreferences() {
  return useOp("get_preferences");
}

/** Expert view: everything is shown (models, roles, diffs, terminal, details). */
export function usePro(): boolean {
  return usePreferences().data?.view === "pro";
}

export function useSetPreferences() {
  const client = useClient();
  const refresh = useRefresh();
  return async (input: OpInput<"set_preferences">) => {
    const p = await client.op("set_preferences", input);
    await refresh("get_preferences");
    return p;
  };
}
