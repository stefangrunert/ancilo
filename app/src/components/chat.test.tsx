import { act, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { renderWithDaemon } from "../test-utils";
import { Composer } from "./Chat";
import { ConversationView } from "./Conversation";
import { CopyAnswer } from "./CopyAnswer";
import { Sidebar } from "./Sidebar";

beforeEach(() => sessionStorage.clear());

// covers: M7-AC-02, M7-AC-10, M7-AC-11
describe("Everyday chat", () => {
  it.each([true, false])("preserves follow-ups in chat drafts and exact retries for other callers (draft=%s)", async (draft) => {
    let reject!: (error: Error) => void;
    const send = vi.fn(() => new Promise<void>((_resolve, fail) => { reject = fail; }));
    renderWithDaemon(<Composer label="Message" draftKey={draft ? "failed-chat" : undefined} onSend={send} />, {});
    await userEvent.type(screen.getByRole("textbox"), "First question{Enter}");
    await userEvent.type(screen.getByRole("textbox"), "Next thought");
    expect(screen.getByRole("button", { name: "Send" })).toBeDisabled();
    await act(async () => reject(new Error("Disconnected")));
    expect(screen.getByRole("textbox")).toHaveValue(draft ? "First question\n\nNext thought" : "First question");
    expect(send).toHaveBeenCalledTimes(1);
  });

  it("keeps separate drafts through navigation and component remounts", async () => {
    const show = (id: string) => renderWithDaemon(<Composer label="Message" draftKey={id} onSend={() => {}} />, {});
    const a = show("first");
    await userEvent.type(screen.getByRole("textbox"), "Unsent first");
    a.unmount();
    const b = show("second");
    expect(screen.getByRole("textbox")).toHaveValue("");
    await userEvent.type(screen.getByRole("textbox"), "Unsent second");
    b.unmount();
    show("first");
    expect(screen.getByRole("textbox")).toHaveValue("Unsent first");
    await userEvent.click(screen.getByRole("button", { name: "Send" }));
    expect(sessionStorage.getItem("ancilo.chat-draft:first")).toBeNull();
  });

  it("keeps a draft when browser storage is unavailable", async () => {
    const get = vi.spyOn(Storage.prototype, "getItem").mockImplementation(() => { throw new Error("Denied"); });
    const set = vi.spyOn(Storage.prototype, "setItem").mockImplementation(() => { throw new Error("Full"); });
    try {
      const first = renderWithDaemon(<Composer label="Message" draftKey="denied" onSend={() => {}} />, {});
      await userEvent.type(screen.getByRole("textbox"), "Keep this");
      first.unmount();
      renderWithDaemon(<Composer label="Message" draftKey="denied" onSend={() => {}} />, {});
      expect(screen.getByRole("textbox")).toHaveValue("Keep this");
    } finally { get.mockRestore(); set.mockRestore(); }
  });

  it("restores the first question after a failure across the empty-chat layout change", async () => {
    renderWithDaemon(<ConversationView id={null} />, {
      pending_actions: () => [],
      ask: () => { throw new Error("No model available"); },
    });
    await userEvent.type(screen.getByRole("textbox"), "Explain rainbows{Enter}");
    await screen.findByText("No model available");
    await waitFor(() => expect(screen.getByRole("textbox")).toHaveValue("Explain rainbows"));
  });

  it("copies the complete answer and reports clipboard failure honestly", async () => {
    const user = userEvent.setup();
    const copy = vi.spyOn(navigator.clipboard, "writeText");
    renderWithDaemon(<CopyAnswer text={"A **useful** answer.\n\n- One\n- Two"} />, {});
    await user.click(screen.getByRole("button", { name: "Copy answer" }));
    expect(copy).toHaveBeenCalledWith("A **useful** answer.\n\n- One\n- Two");
    expect(screen.getByRole("status")).toHaveTextContent("Copied");
    copy.mockRejectedValueOnce(new Error("Denied"));
    await user.click(screen.getByRole("button", { name: "Copy answer" }));
    expect(screen.getByRole("status")).toHaveTextContent("Could not copy");
    copy.mockRestore();
  });

  it("filters all chat titles, including folder chats, and reflects renamed titles", async () => {
    const chats = [
      { id: "c-1", title: "Picnic plans", folder: null },
      { id: "c-2", title: "Recipes", folder: null },
      { id: "c-3", title: "Picnic notes", folder: "/notes" },
    ];
    const { queryClient } = renderWithDaemon(<Sidebar route={{ view: "chat", id: null }} area="chat" onArea={() => {}} showCode={false} />, {
      list_conversations: () => chats,
      list_projects: () => [],
      list_sessions: () => [],
      get_preferences: () => ({ view: "simple", purposes: ["chat"], setup: {}, documents: ["/notes"] }),
    });
    await screen.findByRole("button", { name: "Picnic plans" });
    const list = screen.getByRole("list", { name: "Chats" });
    const filter = screen.getByRole("searchbox", { name: "Filter chat titles" });
    await userEvent.type(filter, "  PICNIC  ");
    expect(within(list).getAllByRole("listitem")).toHaveLength(2);
    expect(within(list).getByRole("button", { name: "Picnic notes" })).toBeVisible();
    expect(within(list).queryByRole("button", { name: "Recipes" })).toBeNull();
    chats[0]!.title = "Weekend plans";
    await act(async () => { await queryClient.invalidateQueries({ queryKey: ["list_conversations"] }); });
    await waitFor(() => expect(within(list).getAllByRole("listitem")).toHaveLength(1));
    await userEvent.type(filter, "{Escape}");
    expect(filter).toHaveValue("");
    expect(within(list).getAllByRole("listitem")).toHaveLength(2);
  });
});
