import "@testing-library/jest-dom/vitest";

// Per-viewer conveniences (area, column width) must not leak between tests.
afterEach(() => {
  try {
    localStorage.clear();
  } catch {
    // no storage – nothing to clear
  }
});
