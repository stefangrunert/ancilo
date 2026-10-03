import { useI18n } from "../i18n";
import { Icon } from "./Icon";

/** The Tasks area (decision `2026-10-03-drei-bereiche`): Ancilo works on the
 * user's files – in a copy; the user keeps or undoes. Being built. */
export function TasksAreaPage() {
  const { t } = useI18n();
  return (
    <div className="page" data-testid="tasks-area">
      <div className="build stack">
      <h1 className="page-title">
        <Icon name="computer" size={24} /> {t("area.tasks")}
      </h1>
      <p>{t("tasksArea.intro")}</p>
      <ul className="examples">
        <li>{t("tasksArea.example1")}</li>
        <li>{t("tasksArea.example2")}</li>
        <li>{t("tasksArea.example3")}</li>
      </ul>
      <p className="muted">{t("tasksArea.safe")}</p>
      <p className="note">{t("tasksArea.soon")}</p>
      </div>
    </div>
  );
}
