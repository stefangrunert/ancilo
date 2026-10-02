import { hrefOf, parseRoute, type Route } from "./route";

describe("routes", () => {
  it.each<[string, Route]>([
    ["", { view: "home" }],
    ["#/", { view: "home" }],
    ["#code", { view: "home" }],
    ["#/chat/new", { view: "chat", id: null }],
    ["#/models", { view: "models" }],
    ["#/build", { view: "build" }],
    ["#/system", { view: "system" }],
    ["#/chat/new/setup", { view: "chat", id: null, kind: "setup" }],
    ["#/chat/new/nonsense", { view: "chat", id: null }],
    ["#/chat/c-1", { view: "chat", id: "c-1" }],
    ["#/project/%2FUsers%2Fme%2Fmy%20proj", { view: "project", root: "/Users/me/my proj" }],
    ["#/session/s-1", { view: "session", id: "s-1" }],
  ])("%s", (hash, route) => {
    expect(parseRoute(hash)).toEqual(route);
  });

  it("round-trip", () => {
    for (const r of [{ view: "home" }, { view: "build" }, { view: "chat", id: null, kind: "write" }, { view: "chat", id: null }, { view: "chat", id: "c-1" }, { view: "project", root: "/a/b c/#x" }, { view: "session", id: "s-1" }] as Route[]) {
      expect(parseRoute(hrefOf(r))).toEqual(r);
    }
  });
});
