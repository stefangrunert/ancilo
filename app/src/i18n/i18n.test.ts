import { de } from "./de";
import { en } from "./en";
import { localize, translate } from "./index";

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

describe("messages from Ancilo", () => {
  it("are shown in the user's language – values carried over, nested ones too", () => {
    expect(
      localize("de", "this model needs about 41.6 GB of memory, but only about 33.0 GB are free right now – close some programs or choose a smaller model"),
    ).toBe("Dieses Modell braucht etwa 41.6 GB Arbeitsspeicher, frei sind gerade nur etwa 33.0 GB – schließe ein paar Programme oder wähle ein kleineres Modell");
    expect(localize("de", "(failed: model call failed: the model ended without an answer)")).toBe(
      "(Das hat nicht geklappt: Die Anfrage an das Modell ist fehlgeschlagen: Das Modell hat keine Antwort geliefert)",
    );
    expect(localize("de", "'qwen' cannot be loaded right now: your computer is short of memory right now – close some programs, then try again – loaded now: a, b")).toBe(
      "„qwen“ lässt sich gerade nicht laden: Auf deinem Computer ist gerade zu wenig Arbeitsspeicher frei – schließe ein paar Programme und versuche es dann noch einmal – gerade geladen: a, b",
    );
    // English as it is; anything unknown as it is.
    expect(localize("en", "download cancelled")).toBe("download cancelled");
    expect(localize("de", "something nobody knows")).toBe("something nobody knows");
  });
});
