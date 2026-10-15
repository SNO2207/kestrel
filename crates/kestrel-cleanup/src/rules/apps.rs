//! Browser and app caches: Chrome, Edge, Firefox, Discord, Teams, Spotify and VS Code. Each is
//! offered for review only while its app is closed (a running app holds its cache open and
//! would write it back at once); while it runs, the recommendation only says so.

use kestrel_index::{EntryId, Index};

use crate::locations::{Locations, join};
use crate::system::any_running;
use crate::tree::{child, find_dir, item_of, name};
use crate::{Claims, Context, Guard, Recommendation, Rule, Tier};

/// Where an app keeps its caches.
enum Place {
    /// A folder that is a cache.
    Folder(String),
    /// A Chromium "User Data" folder: each profile's `Cache`, `Code Cache` and `GPUCache`, and
    /// the shader caches beside the profiles.
    Chromium(String),
    /// Firefox's profiles folder: each profile's `cache2`.
    Firefox(String),
}

struct App {
    id: &'static str,
    name: &'static str,
    programs: &'static [&'static str],
    places: fn(&Locations) -> Vec<Place>,
}

const APPS: &[App] = &[
    App {
        id: "chrome",
        name: "Google Chrome",
        programs: &["chrome.exe"],
        places: |l| vec![Place::Chromium(join(&l.local, r"Google\Chrome\User Data"))],
    },
    App {
        id: "edge",
        name: "Microsoft Edge",
        programs: &["msedge.exe"],
        places: |l| vec![Place::Chromium(join(&l.local, r"Microsoft\Edge\User Data"))],
    },
    App {
        id: "firefox",
        name: "Firefox",
        programs: &["firefox.exe"],
        places: |l| vec![Place::Firefox(join(&l.local, r"Mozilla\Firefox\Profiles"))],
    },
    App {
        id: "discord",
        name: "Discord",
        programs: &["discord.exe"],
        places: |l| ["Cache", "Code Cache", "GPUCache"].iter().map(|n| Place::Folder(join(&l.roaming, &format!(r"discord\{n}")))).collect(),
    },
    App {
        id: "teams",
        name: "Microsoft Teams",
        programs: &["teams.exe", "ms-teams.exe"],
        places: |l| {
            let mut places: Vec<Place> = ["Cache", "Code Cache", "GPUCache", r"Service Worker\CacheStorage"]
                .iter()
                .map(|n| Place::Folder(join(&l.roaming, &format!(r"Microsoft\Teams\{n}"))))
                .collect();
            places.push(Place::Chromium(join(&l.local, r"Packages\MSTeams_8wekyb3d8bbwe\LocalCache\Microsoft\MSTeams\EBWebView")));
            places
        },
    },
    App {
        id: "spotify",
        name: "Spotify",
        programs: &["spotify.exe"],
        places: |l| {
            vec![
                Place::Folder(join(&l.local, r"Spotify\Storage")),
                Place::Folder(join(&l.local, r"Spotify\Data")),
                Place::Folder(join(&l.local, r"Packages\SpotifyAB.SpotifyMusic_zpdnekdrzrea0\LocalCache\Spotify\Data")),
            ]
        },
    },
    App {
        id: "vscode",
        name: "VS Code",
        programs: &["code.exe"],
        places: |l| vec![Place::Folder(join(&l.roaming, r"Code\Cache")), Place::Folder(join(&l.roaming, r"Code\CachedData"))],
    },
];

/// The cache folders of one place that exist.
fn folders(index: &Index, place: &Place) -> Vec<EntryId> {
    match place {
        Place::Folder(path) => find_dir(index, path).into_iter().collect(),
        Place::Chromium(path) => {
            let Some(user_data) = find_dir(index, path) else { return Vec::new() };
            let mut found = Vec::new();
            for entry in index.children(user_data) {
                if !index.is_dir(entry) {
                    continue;
                }
                let profile_caches = ["cache", "code cache", "gpucache"].iter().filter_map(|n| child(index, entry, n));
                found.extend(profile_caches.filter(|&c| index.is_dir(c)));
                if matches!(name(index, entry), "shadercache" | "grshadercache" | "graphitedawncache") {
                    found.push(entry);
                }
            }
            found
        }
        Place::Firefox(path) => {
            let Some(profiles) = find_dir(index, path) else { return Vec::new() };
            index.children(profiles).filter_map(|profile| child(index, profile, "cache2")).filter(|&c| index.is_dir(c)).collect()
        }
    }
}

pub fn recommend(index: &Index, context: &Context, guard: &Guard, claims: &mut Claims) -> Vec<Recommendation> {
    let mut recs = Vec::new();
    for app in APPS {
        let mut items = Vec::new();
        for place in (app.places)(&context.locations) {
            for folder in folders(index, &place) {
                if index.alloc(folder) > 0 && !claims.covers(index, folder) && guard.allows(index, folder) {
                    claims.claim(folder);
                    items.push(item_of(index, folder));
                }
            }
        }
        if items.is_empty() {
            continue;
        }
        let title = format!("{}'s cache", app.name);
        let id = format!("app:{}", app.id);
        let rec = if any_running(&context.running, app.programs) {
            Recommendation::new(
                id,
                Rule::AppCache,
                Tier::Info,
                title,
                format!("{} keeps its cache open while it runs, so it can't be cleared now.", app.name),
            )
            .with_items(items)
            .with_advice(format!("Close {}, then scan again to clear its cache.", app.name))
        } else {
            Recommendation::new(
                id,
                Rule::AppCache,
                Tier::Review,
                title,
                format!(
                    "Pages, images and code {} saved to start and load faster. It fetches them again as you use it, so it may \
                     be slower for a while.",
                    app.name
                ),
            )
            .with_items(items)
            .with_note(format!("Only while {} is closed.", app.name))
        };
        recs.push(rec);
    }
    recs
}

#[cfg(test)]
mod tests {
    use crate::testing::{Sandbox, item_paths};
    use crate::{Action, Tier, scan};

    #[test]
    fn app_caches_are_offered_only_while_their_app_is_closed() {
        let sandbox = Sandbox::new("apps");
        let local = r"Users\me\AppData\Local";
        let roaming = r"Users\me\AppData\Roaming";
        for (rel, seed) in [
            (format!(r"{local}\Google\Chrome\User Data\Default\Cache\Cache_Data\data_1"), 1),
            (format!(r"{local}\Google\Chrome\User Data\Profile 1\Code Cache\js\index"), 2),
            (format!(r"{local}\Google\Chrome\User Data\ShaderCache\data_0"), 3),
            // Never the profile's own data: history, passwords, bookmarks.
            (format!(r"{local}\Google\Chrome\User Data\Default\History"), 4),
            (format!(r"{local}\Microsoft\Edge\User Data\Default\Cache\Cache_Data\data_1"), 5),
            (format!(r"{local}\Mozilla\Firefox\Profiles\abc.default-release\cache2\entries\X"), 6),
            (format!(r"{roaming}\discord\Cache\Cache_Data\f_000001"), 7),
            (format!(r"{roaming}\Microsoft\Teams\Cache\f_1"), 8),
            (format!(r"{local}\Spotify\Data\a\b.file"), 9),
            (format!(r"{roaming}\Code\CachedData\abc\chrome\js\x.code"), 10),
            (format!(r"{roaming}\Code\User\settings.json"), 11),
        ] {
            sandbox.write(&rel, 9000, seed);
        }
        let mut context = sandbox.context();
        context.running = ["msedge.exe".to_owned()].into();
        let recs = scan(&sandbox.index(), &context);
        assert_eq!(
            item_paths(&sandbox, &recs.iter().filter(|r| r.tier == Tier::Review).cloned().collect::<Vec<_>>()),
            [
                "users/me/appdata/local/google/chrome/user data/default/cache",
                "users/me/appdata/local/google/chrome/user data/profile 1/code cache",
                "users/me/appdata/local/google/chrome/user data/shadercache",
                "users/me/appdata/local/mozilla/firefox/profiles/abc.default-release/cache2",
                "users/me/appdata/local/spotify/data",
                "users/me/appdata/roaming/code/cacheddata",
                "users/me/appdata/roaming/discord/cache",
                "users/me/appdata/roaming/microsoft/teams/cache",
            ]
        );
        // Edge runs: shown, with advice, and nothing Kestrel would do.
        let edge = recs.iter().find(|r| r.id == "app:edge").unwrap();
        assert_eq!((edge.tier, &edge.action), (Tier::Info, &Action::None));
        assert_eq!(edge.advice.as_deref(), Some("Close Microsoft Edge, then scan again to clear its cache."));
    }
}
