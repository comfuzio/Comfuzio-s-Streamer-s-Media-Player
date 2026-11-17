// In-app updates via the Tauri updater: checks the signed latest.json of the
// newest GitHub release, downloads, verifies the signature, installs, restarts.

import { useSyncExternalStore } from "react";
import { relaunch } from "@tauri-apps/plugin-process";
import { check, type Update } from "@tauri-apps/plugin-updater";
import { errorMessage } from "./ipc";

export type UpdatePhase = "idle" | "checking" | "upToDate" | "available" | "downloading" | "installing" | "error";

export interface UpdateState {
  phase: UpdatePhase;
  update: Update | null;
  /** Bytes downloaded / total (total null if the server didn't say). */
  progress: { done: number; total: number | null } | null;
  message: string | null;
  checkedAtMs: number | null;
}

let state: UpdateState = { phase: "idle", update: null, progress: null, message: null, checkedAtMs: null };
const listeners = new Set<() => void>();

function set(patch: Partial<UpdateState>) {
  state = { ...state, ...patch };
  listeners.forEach((l) => l());
}

export function useUpdateState(): UpdateState {
  return useSyncExternalStore(
    (l) => { listeners.add(l); return () => listeners.delete(l); },
    () => state,
  );
}

/** Returns the available update, or null when up to date. Never throws. */
export async function checkForUpdate(): Promise<Update | null> {
  if (state.phase === "checking" || state.phase === "downloading" || state.phase === "installing") return state.update;
  set({ phase: "checking", message: null });
  try {
    const update = await check();
    set({
      phase: update ? "available" : "upToDate",
      update,
      checkedAtMs: Date.now(),
      message: update ? null : "You're on the latest version.",
    });
    return update;
  } catch (err) {
    const msg = errorMessage(err);
    // Releases published before the updater existed have no latest.json: not an error.
    const unpublished = /404|not found|valid release JSON/i.test(msg);
    set({
      phase: unpublished ? "upToDate" : "error",
      update: null,
      checkedAtMs: Date.now(),
      message: unpublished
        ? "No update information published yet. Check the GitHub releases page."
        : `Couldn't check for updates: ${msg}`,
    });
    return null;
  }
}

/** Downloads, verifies and installs the update, then restarts the app. */
export async function installUpdate(): Promise<void> {
  const update = state.update;
  if (!update) return;
  let total: number | null = null;
  let done = 0;
  set({ phase: "downloading", progress: { done: 0, total: null }, message: null });
  try {
    await update.downloadAndInstall((event) => {
      if (event.event === "Started") {
        total = event.data.contentLength ?? null;
      } else if (event.event === "Progress") {
        done += event.data.chunkLength;
        set({ progress: { done, total } });
      } else if (event.event === "Finished") {
        set({ phase: "installing" });
      }
    });
    // Windows installers restart the app themselves; elsewhere relaunch now.
    await relaunch();
  } catch (err) {
    set({ phase: "error", message: `Update failed: ${errorMessage(err)}` });
  }
}
