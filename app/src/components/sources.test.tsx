import { screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { renderWithDaemon } from "../test-utils";
import { ConversationView } from "./Conversation";
import { linkEvidence } from "./Sources";

const ev = (id: string, extra: Record<string, unknown> = {}) => ({
  id,
  document: "Verträge/Mietvertrag.pdf",
  origin: { kind: "folder", folder: "/x", path: "Verträge/Mietvertrag.pdf" },
  revision: "a1b2c3d4e5f60718",
  at: { page: 7 },
  part: 6,
  start: 120,
  text: "Das Mietverhältnis kann mit einer Frist von drei Monaten zum Monatsende gekündigt werden.",
  cited: true,
  ...extra,
});

function conversation(evidence: unknown[], dropped: string[] = []) {
  return {
    id: "c-1",
    title: "Kündigung",
    kind: "chat",
    created_at: "2026-10-06T08:00:00Z",
    updated_at: "2026-10-06T08:00:00Z",
    folder: "/x",
    messages: [
      { role: "user", text: "Wie lange ist die Kündigungsfrist?", at: "2026-10-06T08:00:00Z" },
      { role: "assistant", text: "Drei Monate zum Monatsende [D1].", at: "2026-10-06T08:00:01Z", documents: true, evidence, dropped_marks: dropped },
    ],
  };
}

// covers: FPL-01 (sources in the chat)
describe("Sources of an answer from documents", () => {
  it("links only marks of passages the answer was given", () => {
    const e = [ev("D1"), ev("D2")];
    expect(linkEvidence("A [D1]. B [D2]. C [D9]. D [d1].", e as never)).toBe("A [1](#source-D1). B [2](#source-D2). C [D9]. D [1](#source-D1).");
  });

  it("opens the passage the answer had, with its place – and says when the document changed", async () => {
    let now = "same";
    const { calls } = renderWithDaemon(<ConversationView id="c-1" />, {
      get_conversation: () => conversation([ev("D1"), ev("D2", { cited: false })], ["D7"]),
      pending_actions: () => [],
      open_evidence: () => ({ evidence: ev("D1"), now, before: "§ 9 Kündigung. ", after: " § 10 Schönheitsreparaturen" }),
    });
    const answer = await screen.findByTestId("assistant-answer");
    const sources = screen.getByTestId("doc-sources");
    // The cited passage is listed; the one only given is not; a made-up mark is said.
    expect(within(sources).getByTestId("source-D1")).toHaveTextContent("Verträge/Mietvertrag.pdf · page 7");
    expect(within(sources).queryByTestId("source-D2")).toBeNull();
    expect(screen.getByTestId("dropped-marks")).toHaveTextContent("removed 1 source");
    await userEvent.click(within(answer).getByRole("button", { name: "Open source 1" }));
    const view = await screen.findByTestId("source-view");
    await waitFor(() => expect(within(view).getByTestId("source-excerpt")).toHaveTextContent("drei Monaten zum Monatsende"));
    expect(view).toHaveTextContent("§ 9 Kündigung.");
    expect(within(view).getByTestId("source-place")).toHaveTextContent("page 7 · version a1b2c3d4");
    expect(within(view).queryByTestId("source-changed")).toBeNull();
    expect(calls.find((c) => c.op === "open_evidence")?.input).toEqual({ conversation: "c-1", mark: "D1" });
    await userEvent.click(within(view).getByRole("button", { name: "Close" }));
    now = "changed";
    await userEvent.click(within(sources).getByTestId("source-D1"));
    expect(await screen.findByTestId("source-changed")).toHaveTextContent("has changed since the answer");
  });
});
