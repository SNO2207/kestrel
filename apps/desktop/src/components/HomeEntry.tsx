import { HOME_PATH, isHome } from "../lib/home";
import { closeProjects, projectsOpen } from "../projects";
import { activeTab, navigate, openTab } from "../state";
import { ParityIcon } from "./ParityIcon";

/**
 * Home at the top of the sidebar, as in Explorer's navigation pane. Like the Projects entry it
 * is drawn as a place but is not one: it is no folder, and nothing can be dropped on it.
 */
export function HomeEntry() {
  const current = () => !projectsOpen() && isHome(activeTab()?.path) && !activeTab()?.listing;
  const open = (event: MouseEvent) => {
    closeProjects();
    const tab = activeTab();
    if (event.ctrlKey || event.button === 1 || !tab) openTab(HOME_PATH);
    else navigate(tab.id, HOME_PATH);
  };
  return (
    <button
      class="home-entry"
      classList={{ current: current() }}
      title="Home: quick access, favorites and recent files"
      aria-current={current() ? "page" : undefined}
      onClick={open}
      onAuxClick={open}
    >
      <ParityIcon name="home" />
      <span>Home</span>
    </button>
  );
}
