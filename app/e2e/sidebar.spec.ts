import { expect, project, test } from "./harness";

test.use({ daemonArgs: ["--coding"] });

// covers: M8-AC-12
test("projects are renamed and sorted by dragging them in the sidebar", async ({ page, daemon }) => {
  const first = project();
  const second = project();
  await daemon.op("open_project", { path: first });
  await daemon.op("open_project", { path: second });
  await daemon.open(page);
  // Projects are in the Code area.
  await page.getByRole("tab", { name: "Code" }).click();
  const list = page.getByRole("list", { name: "Projects" });
  const names = async () => (await list.locator(":scope > li .nav-item .text").allTextContents());
  const before = await names();
  expect(before).toHaveLength(2);

  // Drag the lower project above the upper one.
  const top = (await list.locator(":scope > li").nth(0).boundingBox())!;
  const lower = (await list.locator(":scope > li").nth(1).boundingBox())!;
  await page.mouse.move(lower.x + 40, lower.y + lower.height / 2);
  await page.mouse.down();
  await page.mouse.move(lower.x + 40, lower.y + lower.height / 2 - 10, { steps: 3 });
  await page.mouse.move(top.x + 40, top.y + 2, { steps: 5 });
  await page.mouse.up();
  await expect.poll(names).toEqual([before[1], before[0]]);
  // Saved, and the drag did not open the project.
  await expect.poll(async () => (await daemon.op("list_projects", {})).map((p: { name: string }) => p.name)).toEqual([before[1], before[0]]);
  expect(page.url()).not.toContain("#/project/");

  // Rename the first one; the folder keeps its name.
  await page.getByRole("button", { name: `Rename ${before[1]}` }).click();
  const field = page.getByRole("textbox", { name: "New title" });
  await field.fill("My shop");
  await field.press("Enter");
  await expect(list.getByRole("button", { name: "My shop", exact: true })).toBeVisible();
  // After a reload the Code area is still open.
  await page.reload();
  await expect.poll(names).toEqual(["My shop", before[0]]);
});

// covers: M10-AC-01
test("the left column is resized by dragging its edge and keeps its width", async ({ page, daemon }) => {
  await daemon.open(page);
  const nav = page.getByRole("navigation", { name: "Navigation" });
  const handle = page.getByRole("separator", { name: "Width of the left column" });
  const width = async () => Math.round((await nav.boundingBox())!.width);
  // Drags the handle by `dx`, starting where it is now.
  const drag = async (dx: number) => {
    const h = (await handle.boundingBox())!;
    const [x, y] = [h.x + h.width / 2, h.y + h.height / 2];
    await page.mouse.move(x, y);
    await page.mouse.down();
    await page.mouse.move(x + dx / 2, y, { steps: 4 });
    await page.mouse.move(x + dx, y, { steps: 4 });
    await page.mouse.up();
  };
  const before = await width();
  await drag(100);
  await expect.poll(width).toBe(before + 100);
  // Narrow (it stops at 200): the areas show only their symbols.
  await drag(-300);
  await expect.poll(width).toBe(200);
  await expect(nav.getByRole("tab", { name: "Chat" }).locator(".text")).toBeHidden();
  await page.reload();
  await expect.poll(width).toBe(200);
});

test("icons sit in the middle of their labels", async ({ page, daemon }) => {
  await daemon.open(page);
  for (const label of ["Set up", "System", "New chat"]) {
    const item = page.getByRole("button", { name: label, exact: true });
    const icon = (await item.locator("svg").boundingBox())!;
    const text = (await item.locator("span").last().boundingBox())!;
    const offset = icon.y + icon.height / 2 - (text.y + text.height / 2);
    expect(Math.abs(offset), `${label}: icon ${offset.toFixed(1)} px off`).toBeLessThan(1.5);
  }
  // The areas: symbol above its name, both centred.
  for (const label of ["Chat", "Tasks"]) {
    const item = page.getByRole("tab", { name: label, exact: true });
    const icon = (await item.locator("svg").boundingBox())!;
    const text = (await item.locator(".text").boundingBox())!;
    const offset = icon.x + icon.width / 2 - (text.x + text.width / 2);
    expect(Math.abs(offset), `${label}: icon ${offset.toFixed(1)} px off`).toBeLessThan(1.5);
    expect(icon.y + icon.height).toBeLessThanOrEqual(text.y + 1);
  }
});
