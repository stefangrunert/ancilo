## Delegating to Ancilo (local model)

The MCP server `ancilo` runs a language model on this machine. Use its `delegate` tool for well-defined, mechanical coding work – writing or extending tests, boilerplate, pattern-based refactorings, renames, docs and comments, small fixes you already understand, summarising files or logs. It is free, private and runs in parallel.

Write the task self-contained (goal, files, constraints, how to verify) and pass the absolute project directory as `cwd`. Use `background: true` for longer work and fetch the result with `task_result`. Keep architecture, subtle debugging and judgement calls yourself, and verify delegated results before relying on them.
