import { For, Show } from "solid-js";

import { isDropPlace } from "../dragdrop";
import { shortcutsApi, type SavedSearch, type Tag, type Workspace } from "../lib/shortcutsApi";
import {
  changeSearches,
  changeTags,
  changeWorkspaces,
  folderName,
  forgetFolder,
  frequent,
  openPlace,
  pinPaths,
  restoreWorkspace,
  runSavedSearch,
  saveWorkspace,
  searchTag,
  searches,
  tagQuery,
  tags,
  workspaces,
} from "../shortcuts";
import { projectsOpen } from "../projects";
import { diskOpen } from "../disk";
import { activeTab, isSearch } from "../state";
import { askName, showMenu } from "./ShortcutDialogs";
import { ShortcutIcon } from "./ShortcutIcon";

const samePath = (a: string, b: string) => a.toLowerCase() === b.toLowerCase();

/** Ctrl-click and middle-click open in a new tab, as they do for links. */
const wantsNewTab = (event: MouseEvent) => event.ctrlKey || event.button === 1;

/** Frequent folders, saved searches, tags and workspaces, below the places and drives. */
export function ShortcutLists() {
  return (
    <>
      <FrequentFolders />
      <SavedSearches />
      <Tags />
      <Workspaces />
    </>
  );
}

function FrequentFolders() {
  const current = () => {
    const tab = activeTab();
    return tab && !isSearch(tab) && !projectsOpen() && !diskOpen() ? tab.path : "";
  };
  const menu = (event: MouseEvent, path: string) => {
    event.preventDefault();
    showMenu(event.clientX, event.clientY, [
      { label: "Open", run: () => openPlace(path, true) },
      { label: "Open in new tab", run: () => openPlace(path, true, true) },
      "separator",
      { label: "Pin to sidebar", run: () => void pinPaths([path]) },
      { label: "Remove from Frequent", run: () => void forgetFolder(path) },
    ]);
  };
  return (
    <Show when={frequent().length > 0}>
      <section class="shelf" aria-label="Frequent">
        <div class="shelf-head">
          <h2>Frequent</h2>
        </div>
        <For each={frequent()}>
          {(folder) => (
            <button
              class="shelf-item frequent"
              classList={{ current: samePath(current(), folder.path), "drop-target": isDropPlace(folder.path) }}
              title={folder.path}
              data-place={folder.path}
              data-place-name={folderName(folder.path)}
              onClick={(event) => openPlace(folder.path, true, wantsNewTab(event))}
              onAuxClick={(event) => event.button === 1 && openPlace(folder.path, true, true)}
              onContextMenu={(event) => menu(event, folder.path)}
            >
              <ShortcutIcon name="folder" />
              <span class="shelf-label">{folderName(folder.path)}</span>
            </button>
          )}
        </For>
      </section>
    </Show>
  );
}

function SavedSearches() {
  const describe = (saved: SavedSearch) => `${saved.query}\n${saved.scope ? `In ${saved.scope}` : "Everywhere"}`;
  const menu = (event: MouseEvent, saved: SavedSearch) => {
    event.preventDefault();
    showMenu(event.clientX, event.clientY, [
      { label: "Search", run: () => void runSavedSearch(saved) },
      { label: "Search in new tab", run: () => void runSavedSearch(saved, true) },
      "separator",
      {
        label: "Rename…",
        run: async () => {
          const answer = await askName({ title: "Rename saved search", label: "Name", initial: saved.name, action: "Rename" });
          if (answer) await changeSearches(() => shortcutsApi.renameSearch(saved.id, answer.name));
        },
      },
      "separator",
      { label: "Delete", danger: true, run: () => void changeSearches(() => shortcutsApi.deleteSearch(saved.id)) },
    ]);
  };
  return (
    <Show when={searches().length > 0}>
      <section class="shelf" aria-label="Saved searches">
        <div class="shelf-head">
          <h2>Searches</h2>
        </div>
        <For each={searches()}>
          {(saved) => (
            <button
              class="shelf-item saved-search"
              classList={{ current: activeTab()?.query === saved.query }}
              title={describe(saved)}
              onClick={(event) => void runSavedSearch(saved, wantsNewTab(event))}
              onAuxClick={(event) => event.button === 1 && void runSavedSearch(saved, true)}
              onContextMenu={(event) => menu(event, saved)}
            >
              <ShortcutIcon name="search" />
              <span class="shelf-label">{saved.name}</span>
            </button>
          )}
        </For>
      </section>
    </Show>
  );
}

function Tags() {
  const newTag = async () => {
    const answer = await askName({ title: "New tag", label: "Name", initial: "", action: "Create", color: { initial: "blue", allowNone: false } });
    if (answer?.color) await changeTags(() => shortcutsApi.createTag(answer.name, answer.color!));
  };
  const menu = (event: MouseEvent, tag: Tag) => {
    event.preventDefault();
    showMenu(event.clientX, event.clientY, [
      { label: "Show tagged items", run: () => searchTag(tag) },
      { label: "Show in new tab", run: () => searchTag(tag, true) },
      "separator",
      {
        label: "Rename and colour…",
        run: async () => {
          const answer = await askName({ title: "Edit tag", label: "Name", initial: tag.name, action: "Save", color: { initial: tag.color, allowNone: false } });
          if (answer) await changeTags(() => shortcutsApi.updateTag(tag.id, { name: answer.name, color: answer.color ?? tag.color }));
        },
      },
      { label: "New tag…", run: () => void newTag() },
      "separator",
      { label: "Delete tag", danger: true, run: () => void changeTags(() => shortcutsApi.deleteTag(tag.id)) },
    ]);
  };
  return (
    <Show when={tags().length > 0}>
      <section class="shelf" aria-label="Tags">
        <div class="shelf-head">
          <h2>Tags</h2>
          <button class="shelf-add" title="New tag" aria-label="New tag" onClick={() => void newTag()}>
            <ShortcutIcon name="plus" size={14} />
          </button>
        </div>
        <For each={tags()}>
          {(tag) => (
            <button
              class="shelf-item tag-item"
              classList={{ current: activeTab()?.query === tagQuery(tag) }}
              title={`Show everything tagged ${tag.name}`}
              onClick={(event) => searchTag(tag, wantsNewTab(event))}
              onAuxClick={(event) => event.button === 1 && searchTag(tag, true)}
              onContextMenu={(event) => menu(event, tag)}
            >
              <span class="tag-dot" data-color={tag.color} />
              <span class="shelf-label">{tag.name}</span>
            </button>
          )}
        </For>
      </section>
    </Show>
  );
}

function Workspaces() {
  const save = async () => {
    const answer = await askName({ title: "Save tabs as a workspace", label: "Name", initial: "", placeholder: "Writing, Taxes 2026…", action: "Save" });
    if (answer) await saveWorkspace(answer.name);
  };
  const menu = (event: MouseEvent, workspace: Workspace) => {
    event.preventDefault();
    event.stopPropagation();
    showMenu(event.clientX, event.clientY, [
      { label: "Open", run: () => restoreWorkspace(workspace) },
      { label: "Save the open tabs here", run: () => void saveWorkspace(workspace.name) },
      "separator",
      {
        label: "Rename…",
        run: async () => {
          const answer = await askName({ title: "Rename workspace", label: "Name", initial: workspace.name, action: "Rename" });
          if (answer) await changeWorkspaces(() => shortcutsApi.renameWorkspace(workspace.id, answer.name));
        },
      },
      "separator",
      { label: "Delete", danger: true, run: () => void changeWorkspaces(() => shortcutsApi.deleteWorkspace(workspace.id)) },
    ]);
  };
  const describe = (workspace: Workspace) => workspace.tabs.map((tab) => tab.path).join("\n");
  return (
    <section class="shelf" aria-label="Workspaces">
      <div class="shelf-head">
        <h2>Workspaces</h2>
        <button class="shelf-add" title="Save tabs as a workspace" aria-label="Save tabs as a workspace" onClick={() => void save()}>
          <ShortcutIcon name="plus" size={14} />
        </button>
      </div>
      <For each={workspaces()} fallback={<p class="shelf-hint">Save the open tabs to come back to them later.</p>}>
        {(workspace) => (
          <button
            class="shelf-item workspace"
            title={describe(workspace)}
            onClick={() => restoreWorkspace(workspace)}
            onContextMenu={(event) => menu(event, workspace)}
          >
            <ShortcutIcon name="workspace" />
            <span class="shelf-label">{workspace.name}</span>
            <span class="shelf-count">{workspace.tabs.length}</span>
          </button>
        )}
      </For>
    </section>
  );
}
