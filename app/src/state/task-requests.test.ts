import { Client } from "../api/client";
import { pendingTaskStart, sendTaskMessage, startTask } from "./task-requests";

beforeEach(() => sessionStorage.clear());

// covers: M10-AC-04, M8-AC-06
describe("task recovery", () => {
  it("reuses creation and message IDs after lost responses, including after a reload", async () => {
    const creates: string[] = [];
    const sends: string[] = [];
    let failCreate = true;
    let failSend = true;
    const client = new Client("", "t", async (url, init) => {
      const input = JSON.parse(String(init?.body));
      if (String(url).endsWith("create_task")) {
        creates.push(input.request_id);
        if (failCreate) { failCreate = false; throw new Error("lost acknowledgement"); }
        return Response.json({ id: "s-1" });
      }
      sends.push(input.request_id);
      if (failSend) { failSend = false; throw new Error("lost acknowledgement"); }
      return Response.json({ id: "s-1" });
    });
    await expect(startTask(client, "Bills", "/bills", [], "Choose again")).rejects.toThrow();
    expect(pendingTaskStart()?.text).toBe("Bills");
    await expect(startTask(client, "Bills", "/bills", [], "Choose again")).rejects.toThrow();
    expect(await startTask(client, "Bills", "/bills", [], "Choose again")).toBe("s-1");
    expect(creates).toHaveLength(2);
    expect(creates[0]).toBe(creates[1]);
    expect(sends).toHaveLength(2);
    expect(sends[0]).toBe(sends[1]);
    expect(pendingTaskStart()).toBeNull();
  });

  it("keeps a partial upload and asks for missing files after reload before sending", async () => {
    const files = [new File(["a"], "a.txt", { lastModified: 1 }), new File(["b"], "b.txt", { lastModified: 2 })];
    const uploads: string[] = [];
    let fail = true;
    let sends = 0;
    const client = new Client("", "t", async (url) => {
      if (String(url).endsWith("create_task")) return Response.json({ id: "s-1" });
      if (String(url).includes("/files?")) {
        const name = new URL(String(url), "http://x").searchParams.get("name")!;
        uploads.push(name);
        if (name === "b.txt" && fail) { fail = false; throw new Error("offline"); }
        return Response.json({ name });
      }
      sends += 1;
      return Response.json({});
    });
    await expect(startTask(client, "Bills", "/bills", files, "Choose again")).rejects.toThrow();
    await expect(startTask(client, "Bills", "/bills", [], "Choose again")).rejects.toThrow("Choose again: b.txt");
    expect(sends).toBe(0);
    await startTask(client, "Bills", "/bills", [files[1]!], "Choose again");
    expect(uploads).toEqual(["a.txt", "b.txt", "b.txt"]);
    expect(sends).toBe(1);
  });

  it("retries an uncertain message once and gives an intentional next message a new ID", async () => {
    const ids: string[] = [];
    let fail = true;
    const client = new Client("", "t", async (_url, init) => {
      ids.push(JSON.parse(String(init?.body)).request_id);
      if (fail) { fail = false; throw new Error("offline"); }
      return Response.json({});
    });
    await expect(sendTaskMessage(client, "s-1", "Continue")).rejects.toThrow();
    await sendTaskMessage(client, "s-1", "Continue");
    await sendTaskMessage(client, "s-1", "Continue");
    expect(ids[0]).toBe(ids[1]);
    expect(ids[2]).not.toBe(ids[1]);
  });
});

it("does not silently ignore extra files added while a request awaits acknowledgement", async () => {
  const client = new Client("", "t", async () => { throw new Error("offline"); });
  await expect(sendTaskMessage(client, "s-1", "Continue")).rejects.toThrow();
  await expect(sendTaskMessage(client, "s-1", "Continue", [new File(["extra"], "extra.txt")])).rejects.toThrow("Retry the pending request before changing its files");
});
