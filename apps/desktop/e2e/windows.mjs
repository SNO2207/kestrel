// What Windows itself shows of the slot's Kestrel window: the webviews it holds, each with
// its visibility and place on screen. A webview's page can't tell whether its view is shown
// (WebView2 leaves `document.visibilityState` alone when a view is hidden), so tests that need
// to know ask the window manager instead.

import { execFile, execFileSync } from "node:child_process";
import { promisify } from "node:util";

import { appExe } from "./slot.mjs";

const SCRIPT = String.raw`
Add-Type @"
using System;
using System.Text;
using System.Collections.Generic;
using System.Runtime.InteropServices;
public static class KestrelE2eWindows {
  public delegate bool EnumProc(IntPtr hwnd, IntPtr lParam);
  [DllImport("user32.dll")] static extern bool EnumWindows(EnumProc cb, IntPtr lParam);
  [DllImport("user32.dll")] static extern bool EnumChildWindows(IntPtr parent, EnumProc cb, IntPtr lParam);
  [DllImport("user32.dll")] static extern uint GetWindowThreadProcessId(IntPtr hwnd, out uint pid);
  [DllImport("user32.dll")] static extern bool IsWindowVisible(IntPtr hwnd);
  [DllImport("user32.dll")] static extern IntPtr GetParent(IntPtr hwnd);
  [DllImport("user32.dll", CharSet = CharSet.Unicode)] static extern int GetClassName(IntPtr hwnd, StringBuilder text, int max);
  [DllImport("user32.dll", CharSet = CharSet.Unicode)] static extern int GetWindowText(IntPtr hwnd, StringBuilder text, int max);
  [StructLayout(LayoutKind.Sequential)] struct Rect { public int Left, Top, Right, Bottom; }
  [DllImport("user32.dll")] static extern bool GetWindowRect(IntPtr hwnd, out Rect rect);
  public static string Webviews(uint pid) {
    var found = new List<string>();
    EnumWindows((top, unused) => {
      uint owner;
      GetWindowThreadProcessId(top, out owner);
      var title = new StringBuilder(64);
      GetWindowText(top, title, 64);
      if (owner != pid || title.ToString() != "Kestrel") return true;
      EnumChildWindows(top, (child, alsoUnused) => {
        var name = new StringBuilder(64);
        GetClassName(child, name, 64);
        if (GetParent(child) != top || name.ToString() != "WRY_WEBVIEW") return true;
        Rect rect;
        GetWindowRect(child, out rect);
        found.Add(String.Format("{{\"visible\":{0},\"left\":{1},\"top\":{2},\"width\":{3},\"height\":{4}}}",
          IsWindowVisible(child) && IsWindowVisible(top) ? "true" : "false", rect.Left, rect.Top, rect.Right - rect.Left, rect.Bottom - rect.Top));
        return true;
      }, IntPtr.Zero);
      return true;
    }, IntPtr.Zero);
    return "[" + String.Join(",", found) + "]";
  }
}
"@
$exe = $env:KESTREL_E2E_EXE
$app = Get-CimInstance Win32_Process -Filter "Name = 'kestrel.exe'" | Where-Object { $_.ExecutablePath -eq $exe } | Select-Object -First 1
if ($app) { [KestrelE2eWindows]::Webviews([uint32]$app.ProcessId) } else { "[]" }
`;

const OTHERS = String.raw`
Add-Type @"
using System;
using System.Text;
using System.Collections.Generic;
using System.Runtime.InteropServices;
public static class KestrelE2eOtherWindows {
  public delegate bool EnumProc(IntPtr hwnd, IntPtr lParam);
  [DllImport("user32.dll")] static extern bool EnumWindows(EnumProc cb, IntPtr lParam);
  [DllImport("user32.dll")] static extern uint GetWindowThreadProcessId(IntPtr hwnd, out uint pid);
  [DllImport("user32.dll")] static extern bool IsWindowVisible(IntPtr hwnd);
  [DllImport("user32.dll", CharSet = CharSet.Unicode)] static extern int GetClassName(IntPtr hwnd, StringBuilder text, int max);
  [DllImport("user32.dll", CharSet = CharSet.Unicode)] static extern int GetWindowText(IntPtr hwnd, StringBuilder text, int max);
  public static string List(uint[] pids) {
    var found = new List<string>();
    EnumWindows((top, unused) => {
      uint owner;
      GetWindowThreadProcessId(top, out owner);
      if (Array.IndexOf(pids, owner) < 0 || !IsWindowVisible(top)) return true;
      var title = new StringBuilder(256);
      GetWindowText(top, title, 256);
      var name = new StringBuilder(256);
      GetClassName(top, name, 256);
      // The app's own windows, and the message window of its event loop.
      if (title.ToString() == "Kestrel" || name.ToString() == "Tao Thread Event Target") return true;
      found.Add(name.ToString() + " \"" + title.ToString() + "\" (process " + owner + ")");
      return true;
    }, IntPtr.Zero);
    return String.Join("\n", found);
  }
}
"@
$dir = Split-Path -Parent $env:KESTREL_E2E_EXE
$exes = @($env:KESTREL_E2E_EXE, (Join-Path $dir 'kestrel-shellhost.exe'))
$pids = @(Get-CimInstance Win32_Process -Filter "Name = 'kestrel.exe' OR Name = 'kestrel-shellhost.exe'" | Where-Object { $exes -contains $_.ExecutablePath } | ForEach-Object { [uint32]$_.ProcessId })
if ($pids.Count) { [KestrelE2eOtherWindows]::List([uint32[]]$pids) }
`;

/**
 * The visible windows of the slot's Kestrel and its Windows menu helper other than the app's own
 * windows: the shell's dialogs (a copy waiting to be told whether to replace a file, Properties),
 * message boxes, menus. Each as `class "title" (process id)`. A page can't see them.
 */
export async function otherWindows(timeout = 15_000) {
  const encoded = Buffer.from(OTHERS, "utf16le").toString("base64");
  const { stdout } = await promisify(execFile)("powershell", ["-NoProfile", "-EncodedCommand", encoded], {
    encoding: "utf8",
    timeout,
    env: { ...process.env, KESTREL_E2E_EXE: appExe },
  });
  return stdout.split(/\r?\n/).filter(Boolean);
}

/**
 * The webviews in the slot's Kestrel window, largest (the app's page) first, each with
 * `visible` and its rectangle on screen in physical pixels.
 */
export function webviews() {
  // Encoded, so no quote in the script is lost on the command line.
  const encoded = Buffer.from(SCRIPT, "utf16le").toString("base64");
  const output = execFileSync("powershell", ["-NoProfile", "-EncodedCommand", encoded], {
    encoding: "utf8",
    // PowerShell's progress records would otherwise reach this process's error stream.
    stdio: ["ignore", "pipe", "ignore"],
    env: { ...process.env, KESTREL_E2E_EXE: appExe },
  });
  return JSON.parse(output.trim() || "[]").sort((a, b) => b.width * b.height - a.width * a.height);
}
