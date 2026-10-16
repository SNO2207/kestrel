import { describing, isDescribeQuery, setDescribing } from "../semantic";
import { activeTab } from "../state";
import { Icon } from "./Icon";

/**
 * The search box's Describe toggle: while it is on, what is typed finds photos and videos by
 * what they show (as a query starting with `~` does) instead of by name.
 */
export function DescribeToggle(props: { focusSearch: () => void }) {
  const on = () => describing() || isDescribeQuery(activeTab()?.query ?? "");
  return (
    <button
      class="describe-toggle"
      classList={{ on: on() }}
      title={on() ? "Describing photos and videos. Click to search by name." : "Describe: find photos and videos by what they show (or start with ~)"}
      aria-label="Describe"
      aria-pressed={on()}
      onClick={() => {
        setDescribing(!on());
        props.focusSearch();
      }}
    >
      <Icon name="sparkle" size={14} />
    </button>
  );
}
