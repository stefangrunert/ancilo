import { expect, project, test } from "./harness";

test.use({ daemonArgs: ["--coding"] });

// covers: M8-AC-12
test("projects are renamed and sorted by dragging them in the sidebar", async ({ page, daemon }) => {
  const first = project();
  const second = project();
  await daemon.op("open_project", { path: first });
  await daemon.op("open_project", { path: second });
  await daemon.open(page);
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
  await page.reload();
  await expect.poll(names).toEqual(["My shop", before[0]]);
});

test("icons sit in the middle of their labels", async ({ page, daemon }) => {
  await daemon.open(page);
  const nav = page.getByRole("navigation", { name: "Navigation" });
  for (const label of ["Set up", "System", "New chat"]) {
    const item = nav.getByRole("button", { name: label, exact: true });
    const icon = (await item.locator("svg").boundingBox())!;
    const text = (await item.locator(".text").boundingBox())!;
    const offset = icon.y + icon.height / 2 - (text.y + text.height / 2);
    expect(Math.abs(offset), `${label}: icon ${offset.toFixed(1)} px off`).toBeLessThan(1.5);
  }
});
