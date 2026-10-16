import type { GroupBy } from "../lib/layoutApi";
import { groupAt, locationKey } from "../lib/views";
import { setGrouping, tabs } from "../state";
import type { MenuEntry, MenuItem } from "./ContextMenu";

const GROUPS: { by: GroupBy; label: string }[] = [
  { by: "name", label: "Name" },
  { by: "modified", label: "Date modified" },
  { by: "type", label: "Type" },
  { by: "size", label: "Size" },
];

/** Dates and sizes group newest and largest first, as Explorer starts them. */
const startsDescending = (by: GroupBy) => by === "modified" || by === "size";

/**
 * Explorer's "Group by" for a tab, as a submenu: none or one of the four groupings, and the
 * order of the groups. Used by the View menu and the folder's own menu.
 */
export function groupByEntry(tabId: number): MenuItem {
  const tab = tabs.find((each) => each.id === tabId);
  const current = tab ? groupAt(locationKey(tab)) : null;
  const entries: MenuEntry[] = [
    { label: "None", checked: current === null, run: () => setGrouping(tabId, null) },
    ...GROUPS.map(
      ({ by, label }): MenuEntry => ({
        label,
        checked: current?.by === by,
        run: () => setGrouping(tabId, { by, desc: current?.by === by ? current.desc : startsDescending(by) }),
      }),
    ),
    "separator",
    {
      label: "Ascending",
      checked: current !== null && !current.desc,
      disabled: current === null,
      run: () => current && setGrouping(tabId, { by: current.by, desc: false }),
    },
    {
      label: "Descending",
      checked: current !== null && current.desc,
      disabled: current === null,
      run: () => current && setGrouping(tabId, { by: current.by, desc: true }),
    },
  ];
  return { label: "Group by", run: () => {}, submenu: entries };
}
