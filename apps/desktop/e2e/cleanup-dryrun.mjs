// A read-only dry run of cleanup recommendations and the duplicate finder on this machine's
// real drive, through a running slot. It changes nothing: it scans, asks what moving every
// offered item to the Recycle Bin would do (dry run), lists the tools that would run without
// running them, and looks for duplicates of 10 MB or more in the user folder (hashes are kept
// in the slot's own database). It prints a summary.
//
// `KESTREL_SLOT=<n> node e2e/cleanup-dryrun.mjs`. Not a test suite: what it finds depends on
// the machine. Never pass anything here that would act for real.

import { connectToKestrel } from "./connect.mjs";

const { browser, page } = await connectToKestrel();
const invoke = (command, args = {}) =>
  page.evaluate(([command, args]) => window.__TAURI_INTERNALS__.invoke(command, args), [command, args]);
const size = (bytes) => {
  const units = ["B", "KB", "MB", "GB", "TB"];
  let value = bytes;
  let unit = 0;
  while (value >= 1024 && unit < units.length - 1) {
    value /= 1024;
    unit += 1;
  }
  return `${value >= 100 || unit === 0 ? value.toFixed(0) : value.toFixed(1)} ${units[unit]}`;
};

try {
  // The real machine, not a test's scratch folder.
  await invoke("cleanup_dev_sandbox", { sandbox: null });
  const started = Date.now();
  const report = await invoke("cleanup_scan", { root: null });
  console.log(`Scan: ${report.recommendations.length} recommendations in ${report.took_ms} ms (call ${Date.now() - started} ms); ${size(report.total)} could be freed by the Safe and Review ones${report.indexing ? " (index still building)" : ""}.`);
  console.log("");
  for (const rec of report.recommendations) {
    const action = rec.action.kind === "tool" ? `tool: ${rec.action.command}${rec.action.dir ? ` in ${rec.action.dir}` : ""}` : rec.action.kind;
    console.log(`[${rec.tier}] ${rec.title}: ${size(rec.bytes)}, ${rec.items.length} items, ${action}`);
    for (const part of rec.parts) console.log(`        ${part.label}: ${size(part.bytes)}`);
  }

  // What moving every offered item to the Recycle Bin would do. A dry run only: nothing moves.
  const chosen = report.recommendations
    .filter((rec) => rec.action.kind === "recycle" && rec.tier !== "info")
    .map((rec) => ({ id: rec.id, paths: rec.items.map((item) => item.path) }));
  const dry = await invoke("cleanup_recycle", { chosen, dryRun: true });
  if (!dry.dry_run) throw new Error("expected a dry run");
  console.log("");
  console.log(`Dry run: ${dry.message}`);
  const reasons = new Map();
  for (const skipped of dry.skipped) reasons.set(skipped.reason, (reasons.get(skipped.reason) ?? 0) + 1);
  for (const [reason, count] of reasons) console.log(`        skipped because ${reason}: ${count}`);

  for (const rec of report.recommendations.filter((rec) => rec.action.kind === "tool")) {
    const plan = await invoke("cleanup_run_tool", { id: rec.id, dryRun: true });
    if (!plan.dry_run) throw new Error("expected a dry run");
    console.log(`Would run: ${plan.command}${plan.dir ? ` in ${plan.dir}` : ""}`);
  }

  // Duplicates of 10 MB or more in the user folder, without AppData. Reading only.
  const defaults = await invoke("dupes_defaults");
  const cancel = setTimeout(() => void invoke("dupes_cancel").catch(() => {}), 5 * 60_000);
  const dupesStarted = Date.now();
  const dupes = await invoke("dupes_scan", { request: { roots: defaults.roots, exclude: defaults.exclude, min_size: 10 << 20 } });
  clearTimeout(cancel);
  console.log("");
  if (!dupes) {
    console.log("Duplicates: the search took over five minutes and was cancelled.");
  } else {
    console.log(
      `Duplicates in ${defaults.roots.join(", ")} (without ${defaults.exclude.join(", ")}), 10 MB or more: ${dupes.groups.length} groups, ${size(dupes.wasted)} in extra copies; ` +
        `${dupes.candidates} candidates, ${dupes.hashed_files} files hashed (${size(dupes.hashed_bytes)} read), ${dupes.reused} from the cache, ` +
        `${dupes.placeholders} online-only files left alone, ${dupes.skipped} skipped; ${Date.now() - dupesStarted} ms.`,
    );
    for (const group of dupes.groups.slice(0, 5)) {
      console.log(`        ${group.files.length} copies of ${size(group.size)}: ${size(group.wasted)} extra`);
    }
  }
} finally {
  await browser.close();
}
