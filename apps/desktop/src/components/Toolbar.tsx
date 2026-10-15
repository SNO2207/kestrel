import { For, Show, createEffect, createSignal } from "solid-js";

import { recheckAccess } from "../lib/access";
import { breadcrumbs } from "../lib/format";
import {
  activeTab,
  canGoBack,
  canGoForward,
  canGoUp,
  everywhere,
  goBack,
  goForward,
  goUp,
  navigate,
  refresh,
  setEverywhere,
  setQuery,
} from "../state";
import { Icon } from "./Icon";

/** Lets the app's keyboard shortcuts focus the address and search fields. */
export const focusTargets: { address?: () => void; search?: () => void } = {};

export function Toolbar(props: { focusList: () => void }) {
  const [editing, setEditing] = createSignal(false);
  let addressInput: HTMLInputElement | undefined;
  let searchInput: HTMLInputElement | undefined;

  focusTargets.address = () => setEditing(true);
  focusTargets.search = () => {
    searchInput?.focus();
    searchInput?.select();
  };

  createEffect(() => {
    if (editing()) {
      addressInput?.focus();
      addressInput?.select();
    }
  });

  const tab = activeTab;
  const go = (path: string) => {
    const current = tab();
    if (current && path.trim()) navigate(current.id, path);
    setEditing(false);
    props.focusList();
  };

  return (
    <div class="toolbar">
      <div class="nav">
        <button title="Back (Alt+Left)" aria-label="Back" disabled={!tab() || !canGoBack(tab()!)} onClick={() => goBack(tab()!.id)}>
          <Icon name="back" />
        </button>
        <button title="Forward (Alt+Right)" aria-label="Forward" disabled={!tab() || !canGoForward(tab()!)} onClick={() => goForward(tab()!.id)}>
          <Icon name="forward" />
        </button>
        <button title="Up (Alt+Up)" aria-label="Up" disabled={!tab() || !canGoUp(tab()!)} onClick={() => goUp(tab()!.id)}>
          <Icon name="up" />
        </button>
        <button title="Refresh (F5)" aria-label="Refresh" disabled={!tab()} onClick={() => (recheckAccess(), refresh(tab()!.id))}>
          <Icon name="refresh" />
        </button>
      </div>

      <div class="address" classList={{ editing: editing() }} onClick={() => setEditing(true)}>
        <Show
          when={editing()}
          fallback={
            <div class="crumbs">
              <For each={breadcrumbs(tab()?.path ?? "")}>
                {(crumb, i) => (
                  <>
                    <Show when={i() > 0}>
                      <Icon name="chevron" size={12} />
                    </Show>
                    <button
                      class="crumb"
                      onClick={(event) => {
                        event.stopPropagation();
                        go(crumb.path);
                      }}
                    >
                      {crumb.label}
                    </button>
                  </>
                )}
              </For>
            </div>
          }
        >
          <input
            ref={addressInput}
            class="address-input"
            aria-label="Address"
            spellcheck={false}
            value={tab()?.path ?? ""}
            onBlur={() => setEditing(false)}
            onKeyDown={(event) => {
              if (event.key === "Enter") go(event.currentTarget.value);
              if (event.key === "Escape") {
                setEditing(false);
                props.focusList();
              }
            }}
          />
        </Show>
      </div>

      <div class="searchbox">
        <Icon name="search" size={14} />
        <input
          ref={searchInput}
          aria-label="Search"
          placeholder={everywhere() ? "Search everywhere" : "Search this folder"}
          spellcheck={false}
          value={tab()?.query ?? ""}
          onInput={(event) => tab() && setQuery(tab()!.id, event.currentTarget.value)}
          onKeyDown={(event) => {
            if (event.key === "Escape") {
              if (tab()) setQuery(tab()!.id, "");
              props.focusList();
            }
            if (event.key === "Enter" || event.key === "ArrowDown") {
              event.preventDefault();
              props.focusList();
            }
          }}
        />
        <button
          class="scope"
          classList={{ on: everywhere() }}
          title={everywhere() ? "Searching every indexed drive. Click to search only this folder." : "Searching this folder. Click to search every indexed drive."}
          aria-label="Search everywhere"
          aria-pressed={everywhere()}
          onClick={() => setEverywhere(!everywhere())}
        >
          <Icon name={everywhere() ? "globe" : "folder"} size={14} />
        </button>
      </div>
    </div>
  );
}
