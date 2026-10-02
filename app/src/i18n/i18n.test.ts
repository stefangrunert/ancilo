import { de } from "./de";
import { en } from "./en";
import { translate } from "./index";

describe("translations", () => {
  it("have the same keys and no empty texts", () => {
    expect(Object.keys(de).sort()).toEqual(Object.keys(en).sort());
    for (const [k, v] of Object.entries(de)) expect(v, k).not.toBe("");
  });

  it("keep placeholders in both languages", () => {
    const vars = (s: string) => (s.match(/\{\w+\}/g) ?? []).sort();
    for (const k of Object.keys(en) as (keyof typeof en)[]) expect(vars(de[k]), k).toEqual(vars(en[k]));
  });

  it("fills placeholders", () => {
    expect(translate("de", "sections.models", { n: 3 })).toBe("Modelle (3)");
    expect(translate("en", "model.tokens", { tps: 42 })).toBe("42 tok/s");
  });
});
