//! Keeps the app's window on the app's own page.
//!
//! Nothing in Kestrel navigates its window anywhere else: web links open in the default browser
//! (`preview::open_link`), and previewed content is sanitised or sandboxed so that it cannot
//! navigate. Should something still try, such as markup the Markdown sanitiser let through, the
//! window refuses to go. The window has no title bar or back button, so a page that replaced the
//! app would leave the user stranded on it, and it could pass itself off as Kestrel.
//!
//! Only the top-level page is guarded; frames (the preview pane's PDF viewer) navigate freely
//! inside the page. The first load of the page is never checked: Tauri asks plugins only once
//! the window exists.

use tauri::plugin::{Builder, TauriPlugin};
use tauri::utils::config::FrontendDist;
use tauri::{Config, Manager, Runtime, Url};

/// The plugin that refuses every top-level navigation away from the app's own origin.
pub fn guard<R: Runtime>() -> TauriPlugin<R> {
    Builder::new("navigation-guard")
        .on_navigation(|webview, url| {
            let allowed = homes(webview.config()).iter().any(|home| same_origin(url, home));
            if !allowed {
                eprintln!("refused to navigate the window away from the app, to {url}");
            }
            allowed
        })
        .build()
}

/// Where the app's page may come from: the dev server while developing, otherwise the app's
/// own protocol, as Tauri serves it on Windows.
fn homes(config: &Config) -> Vec<Url> {
    if tauri::is_dev()
        && let Some(dev) = &config.build.dev_url
    {
        return vec![dev.clone()];
    }
    if let Some(FrontendDist::Url(remote)) = &config.build.frontend_dist {
        return vec![remote.clone()];
    }
    ["http://tauri.localhost/", "https://tauri.localhost/"].into_iter().filter_map(|home| Url::parse(home).ok()).collect()
}

/// Whether `url` is on the same scheme, host and port as `home`. Opaque origins (`data:`,
/// `about:blank`, `file:` on Windows) never are.
fn same_origin(url: &Url, home: &Url) -> bool {
    url.origin().is_tuple() && url.origin() == home.origin()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn url(text: &str) -> Url {
        text.parse().unwrap()
    }

    #[test]
    fn only_the_apps_own_origin_is_home() {
        let dev = url("http://localhost:1490/");
        assert!(same_origin(&url("http://localhost:1490/"), &dev));
        assert!(same_origin(&url("http://localhost:1490/index.html?x=1#y"), &dev), "any page of the app");
        for away in [
            "https://attacker.example/phish",
            "http://localhost:1491/",
            "https://localhost:1490/",
            "http://preview.localhost/?p=C%3A%5Cx.png",
            "file:///C:/Users/someone/report.html",
            "about:blank",
            "data:text/html,<p>hi</p>",
        ] {
            assert!(!same_origin(&url(away), &dev), "{away}");
        }
    }

    #[test]
    fn home_is_the_dev_server_while_developing() {
        let mut config = Config::default();
        config.build.dev_url = Some(url("http://localhost:1490"));
        // Tests are built as in development, without Tauri's `custom-protocol` feature.
        assert!(tauri::is_dev());
        assert_eq!(homes(&config), [url("http://localhost:1490")]);
        config.build.dev_url = None;
        assert_eq!(homes(&config), [url("http://tauri.localhost/"), url("https://tauri.localhost/")]);
    }
}
