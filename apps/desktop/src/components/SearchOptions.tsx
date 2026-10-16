import { SIZE_LIMITS, searchOptions, setSearchOptions } from "../search";
import type { MenuEntry } from "./ContextMenu";
import { SearchIcon } from "./SearchIcon";
import { showMenu } from "./ShortcutDialogs";

const megabytes = (size: number) => (size >= 1000 ? `${size / 1000} GB` : `${size} MB`);

/** The search box's options, in a menu below the button. */
function openOptions(button: HTMLElement) {
  const box = button.getBoundingClientRect();
  const options = searchOptions();
  const entries: MenuEntry[] = [
    { label: "Search inside files", checked: options.inside, run: () => setSearchOptions({ inside: !options.inside }) },
    "separator",
    { label: "Regular expression", checked: options.regex, run: () => setSearchOptions({ regex: !options.regex }) },
    { label: "Match case", checked: options.match_case, run: () => setSearchOptions({ match_case: !options.match_case }) },
    { label: "Whole words", checked: options.whole_word, run: () => setSearchOptions({ whole_word: !options.whole_word }) },
    { label: "Skip what .gitignore ignores", checked: options.gitignore, run: () => setSearchOptions({ gitignore: !options.gitignore }) },
    {
      label: `Skip files over ${megabytes(options.max_size_mb)}`,
      run: () => {},
      submenu: SIZE_LIMITS.map((size) => ({
        label: megabytes(size),
        checked: options.max_size_mb === size,
        run: () => setSearchOptions({ max_size_mb: size }),
      })),
    },
  ];
  showMenu(Math.max(8, box.right - 260), box.bottom + 4, entries);
}

/**
 * The search box's buttons for searching inside files: the switch itself, and a menu of how
 * (regular expressions, case, whole words, .gitignore, the size limit), which also apply to
 * `content:` clauses typed in a search.
 */
export function SearchOptionsButtons() {
  return (
    <>
      <button
        class="search-inside"
        classList={{ on: searchOptions().inside }}
        title={searchOptions().inside ? "Searching inside files. Click to search names." : "Search inside files"}
        aria-label="Search inside files"
        aria-pressed={searchOptions().inside}
        onClick={() => setSearchOptions({ inside: !searchOptions().inside })}
      >
        <SearchIcon name="inside" size={14} />
      </button>
      <button
        class="search-options-button"
        title="Search options"
        aria-label="Search options"
        aria-haspopup="menu"
        onClick={(event) => openOptions(event.currentTarget)}
      >
        <SearchIcon name="options" size={14} />
      </button>
    </>
  );
}

/** What the search box says before anything is typed. */
export function searchPlaceholder(everywhere: boolean): string {
  if (searchOptions().inside) return everywhere ? "Search inside files" : "Search inside files here";
  return everywhere ? "Search everywhere" : "Search this folder";
}
