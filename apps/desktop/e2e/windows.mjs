// What Windows itself shows of the slot's Kestrel window: the webviews it holds, each with
// its visibility and place on screen. A webview's page can't tell whether its view is shown
// (WebView2 leaves `document.visibilityState` alone when a view is hidden), so tests that need
// to know ask the window manager instead.

import { execFileSync } from "node:child_process";

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
