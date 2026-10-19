import type { Client } from "../api/client";

const START = "ancilo.task-start";
interface PendingStart {
  folder: string;
  text: string;
  createId: string;
  sendId: string;
  session?: string;
  files: { name: string; size: number }[];
  uploaded: number;
}

// Keep recovery data in this window only; storage may be disabled.
const memory = new Map<string, string>();
function read<T>(key: string): T | null {
  try {
    const value = sessionStorage.getItem(key);
    return value ? JSON.parse(value) as T : null;
  } catch {
    try { return JSON.parse(memory.get(key) ?? "null") as T | null; } catch { return null; }
  }
}
function write(key: string, value: unknown) {
  const text = JSON.stringify(value);
  memory.set(key, text);
  try { sessionStorage.setItem(key, text); } catch { /* in-memory recovery still works */ }
}
function clear(key: string) {
  memory.delete(key);
  try { sessionStorage.removeItem(key); } catch { /* storage unavailable */ }
}
export const pendingTaskStart = () => read<PendingStart>(START);

export class PendingFilesChanged extends Error {}
function checkSelection(wanted: { name: string; size: number }[], files: File[]) {
  if (files.some((f) => !wanted.some((w) => w.name === f.name && w.size === f.size))) {
    throw new PendingFilesChanged("Retry the pending request before changing its files");
  }
}

/** An uncertain reply is retried with the same ID, including after reload. */
export async function sendTaskMessage(client: Client, session: string, text: string, files: File[] = [], missingFiles = "Please choose the remaining input file again") {
  const key = `ancilo.task-message:${session}`;
  let request = read<{ text: string; id: string; files: { name: string; size: number }[]; uploaded: number }>(key);
  if (!request || request.text !== text) request = { text, id: crypto.randomUUID(), files: files.map((f) => ({ name: f.name, size: f.size })), uploaded: 0 };
  checkSelection(request.files, files);
  write(key, request);
  while (request.uploaded < request.files.length) {
    const wanted = request.files[request.uploaded]!;
    const file = files.find((f) => f.name === wanted.name && f.size === wanted.size);
    if (!file) throw new Error(`${missingFiles}: ${wanted.name}`);
    await client.addTaskFile(session, file.name, file);
    request.uploaded += 1;
    write(key, request);
  }
  const result = await client.op("send_message", { session, text, request_id: request.id });
  clear(key);
  return result;
}

export const pendingTaskMessage = (session: string) => read<{ text: string; id: string }>(`ancilo.task-message:${session}`)?.text ?? "";

/** Resume a partially completed start; uploaded files and an accepted turn
 * are never silently duplicated after a lost response. */
export async function startTask(client: Client, text: string, folder: string, files: File[], missingFiles: string) {
  let pending = pendingTaskStart();
  if (!pending || pending.folder !== folder || pending.text !== text) {
    pending = { folder, text, createId: crypto.randomUUID(), sendId: crypto.randomUUID(), files: files.map((f) => ({ name: f.name, size: f.size })), uploaded: 0 };
    write(START, pending);
  }
  checkSelection(pending.files, files);
  if (!pending.session) {
    const s = await client.op("create_task", { folder, title: (text.trim().split("\n")[0] ?? "").slice(0, 60), request_id: pending.createId });
    pending.session = s.id;
    write(START, pending);
  }
  while (pending.uploaded < pending.files.length) {
    const wanted = pending.files[pending.uploaded]!;
    const f = files.find((f) => f.name === wanted.name && f.size === wanted.size);
    if (!f) throw new Error(`${missingFiles}: ${wanted.name}`);
    await client.addTaskFile(pending.session, f.name, f);
    pending.uploaded += 1;
    write(START, pending);
  }
  await client.op("send_message", { session: pending.session, text: pending.text, request_id: pending.sendId });
  clear(START);
  return pending.session;
}
