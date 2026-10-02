import { useI18n } from "../i18n";
import { useLive, useOp } from "../state/store";
import { Section, StatusDot } from "./ui";

export function TasksSection() {
  const { t } = useI18n();
  const tasks = useOp("list_tasks", { limit: 20 });
  const live = useLive();
  const list = tasks.data ?? [];
  const running = list.filter((x) => x.status === "running" || x.status === "queued").length;
  if (list.length === 0) return null;
  return (
    <Section title={t("sections.tasks")} badge={running ? t("sections.running", { n: running }) : undefined} testId="section-tasks">
      <ul className="tasks">
        {list.map((task) => (
          <li key={task.task_id}>
            <details>
              <summary>
                <StatusDot status={task.status} />
                <span>{task.summary?.split("\n")[0] ?? task.task_id}</span>
                <span className="muted">
                  {task.model}
                  {task.diff_stat ? ` · ${task.diff_stat}` : ""}
                </span>
              </summary>
              <div className="log" aria-label={t("tasks.log")}>
                {(live.activity[task.task_id] ?? []).map((l, i) => (
                  <div key={i}>{l}</div>
                ))}
                {task.branch && <div className="muted">{task.next_step ?? task.branch}</div>}
              </div>
            </details>
          </li>
        ))}
      </ul>
    </Section>
  );
}
