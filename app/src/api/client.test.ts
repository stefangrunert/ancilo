import { ApiError, Client, OfflineError, readToken } from "./client";

function response(body: unknown, status = 200) {
  return new Response(JSON.stringify(body), { status, headers: { "content-type": "application/json" } });
}

describe("Client.op", () => {
  it("posts the input with the token and returns the output", async () => {
    const calls: [string, RequestInit][] = [];
    const client = new Client("http://x", "tok", async (url, init) => {
      calls.push([String(url), init ?? {}]);
      return response([{ id: "m1" }]);
    });
    const out = await client.op("list_models");
    expect(out).toEqual([{ id: "m1" }]);
    const [url, init] = calls[0]!;
    expect(url).toBe("http://x/api/v1/ops/list_models");
    expect((init.headers as Record<string, string>).authorization).toBe("Bearer tok");
    expect((init.headers as Record<string, string>)["x-ancilo-confirm"]).toBeUndefined();
  });

  it("sends the confirmation only when asked", async () => {
    let headers: Record<string, string> = {};
    const client = new Client("", "t", async (_u, init) => {
      headers = init?.headers as Record<string, string>;
      return response({});
    });
    await client.op("remove_model", { model: "m", keep_files: false }, true);
    expect(headers["x-ancilo-confirm"]).toBe("true");
  });

  it("maps API errors and unreachable daemons", async () => {
    const failing = new Client("", "t", async () => response({ error: { code: "not_found", message: "no model 'x'" } }, 404));
    await expect(failing.op("model_status", { model: "x" })).rejects.toMatchObject({ code: "not_found", message: "no model 'x'", status: 404 });
    await expect(failing.op("model_status", { model: "x" })).rejects.toBeInstanceOf(ApiError);
    const offline = new Client("", "t", async () => {
      throw new TypeError("fetch failed");
    });
    await expect(offline.op("list_models")).rejects.toBeInstanceOf(OfflineError);
  });
});

describe("Client.events", () => {
  it("parses the stream, resumes after the last event and reports its state", async () => {
    const urls: string[] = [];
    let round = 0;
    const client = new Client("", "t", async (url) => {
      urls.push(String(url));
      round += 1;
      const text =
        round === 1
          ? `: keep-alive\n\nid: 7\nevent: model.added\ndata: {"seq":7,"ts":"x","kind":"model.added","subject":"m","data":{}}\n\n`
          : `data: {"seq":8,"ts":"x","kind":"instance.ready","subject":"m","data":{}}\n\n`;
      return new Response(new TextEncoder().encode(text), { status: 200 });
    });
    const events: string[] = [];
    const states: boolean[] = [];
    const stop = client.events(
      (e) => events.push(`${e.seq}:${e.kind}`),
      (up) => states.push(up),
    );
    await vi.waitFor(() => expect(events).toEqual(["7:model.added", "8:instance.ready"]), { timeout: 3000 });
    stop();
    expect(urls[1]).toBe("/api/v1/events?after=7");
    expect(states).toContain(true);
    expect(states).toContain(false);
  });
});

describe("readToken", () => {
  it("takes the token from the fragment and removes it from the address", () => {
    sessionStorage.clear();
    history.replaceState(null, "", "/app/#token=abc&x=1");
    expect(readToken()).toBe("abc");
    expect(window.location.hash).toBe("#x=1");
    // Remembered for reloads in this session.
    expect(readToken()).toBe("abc");
  });

  it("prefers the token injected by the native shell", () => {
    window.__ANCILO__ = { token: "native" };
    expect(readToken()).toBe("native");
    delete window.__ANCILO__;
  });
});
