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
  const before = (await nav.boundingBox())!.width;
  const h = (await handle.boundingBox())!;
  await page.mouse.move(h.x + h.width / 2, h.y + h.height / 2);
  await page.mouse.down();
  await page.mouse.move(h.x + h.width / 2 + 100, h.y + h.height / 2, { steps: 5 });
  await page.mouse.up();
  await expect.poll(async () => Math.round((await nav.boundingBox())!.width)).toBe(Math.round(before + 100));
  // Narrow: the areas show only their symbols.
  await page.mouse.move(h.x + 100 + h.width / 2, h.y + h.height / 2);
  await page.mouse.down();
  await page.mouse.move(h.x - 200, h.y + h.height / 2, { steps: 5 });
  await page.mouse.up();
  await expect(nav.getByRole("tab", { name: "Chat" }).locator(".text")).toBeHidden();
  const narrow = Math.round((await nav.boundingBox())!.width);
  expect(narrow).toBe(200);
  await page.reload();
  await expect.poll(async () => Math.round((await nav.boundingBox())!.width)).toBe(narrow);
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
