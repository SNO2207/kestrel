//! Package managers' caches: pnpm's store, npm's cache, Cargo's downloads, pip's, NuGet's,
//! Gradle's, Go's build cache, Yarn's and the Hugging Face hub. Each is offered through the
//! package manager's own command where it has one and it is installed; otherwise what lies
//! inside the cache folder goes to the Recycle Bin (the folder itself stays, as tools expect).

use kestrel_index::Index;

use crate::locations::{Locations, join};
use crate::tree::{find_dir, item_of};
use crate::{Claims, Context, Guard, Item, Recommendation, Rule, Tier, Tool};

struct Cache {
    id: &'static str,
    title: &'static str,
    reason: &'static str,
    tier: Tier,
    /// The cache folders.
    folders: fn(&Locations) -> Vec<String>,
    /// The package manager's own cleanup: program and arguments.
    tool: Option<(&'static str, &'static [&'static str])>,
}

const CACHES: &[Cache] = &[
    Cache {
        id: "pnpm",
        title: "pnpm store",
        reason: "Packages pnpm keeps to link into projects' node_modules. Pruning removes those no project uses any more.",
        tier: Tier::Safe,
        folders: |l| vec![join(&l.local, r"pnpm\store")],
        tool: Some(("pnpm", &["store", "prune"])),
    },
    Cache {
        id: "npm",
        title: "npm cache",
        reason: "Packages npm downloaded. npm downloads them again when a project needs them.",
        tier: Tier::Safe,
        folders: |l| vec![l.npm_cache.clone()],
        tool: None,
    },
    Cache {
        id: "cargo",
        title: "Cargo's download cache",
        reason: "Crates Cargo downloaded and git dependencies it checked out. Cargo fetches them again when a project needs them.",
        tier: Tier::Safe,
        folders: |l| vec![join(&l.cargo_home, r"registry\cache"), join(&l.cargo_home, r"git\checkouts")],
        tool: None,
    },
    Cache {
        id: "pip",
        title: "pip cache",
        reason: "Python packages pip downloaded and built. pip downloads them again when it needs them.",
        tier: Tier::Safe,
        folders: |l| vec![l.pip_cache.clone()],
        tool: Some(("pip", &["cache", "purge"])),
    },
    Cache {
        id: "nuget",
        title: "NuGet caches",
        reason: "Packages NuGet downloaded for .NET projects. They are restored again the next time a project builds.",
        tier: Tier::Safe,
        folders: |l| vec![l.nuget_packages.clone(), join(&l.local, r"NuGet\v3-cache"), join(&l.local, r"NuGet\plugins-cache")],
        tool: Some(("dotnet", &["nuget", "locals", "all", "--clear"])),
    },
    Cache {
        id: "gradle",
        title: "Gradle caches",
        reason: "Dependencies and build caches Gradle keeps for Java and Android projects. Gradle downloads them again when a build needs them.",
        tier: Tier::Safe,
        folders: |l| vec![join(&l.gradle_home, "caches")],
        tool: None,
    },
    Cache {
        id: "go",
        title: "Go build cache",
        reason: "Compiled packages Go keeps to build faster. Go rebuilds them when it needs them.",
        tier: Tier::Safe,
        folders: |l| vec![l.go_cache.clone()],
        tool: Some(("go", &["clean", "-cache"])),
    },
    Cache {
        id: "yarn",
        title: "Yarn cache",
        reason: "Packages Yarn downloaded. Yarn downloads them again when a project needs them.",
        tier: Tier::Safe,
        folders: |l| vec![join(&l.local, r"Yarn\Cache"), join(&l.local, r"Yarn\Berry\cache")],
        tool: None,
    },
    Cache {
        id: "huggingface",
        title: "Hugging Face models and datasets",
        reason: "Models and datasets downloaded by Hugging Face libraries. A program downloads them again when it needs \
                 them, which can take long: models are big downloads.",
        tier: Tier::Review,
        folders: |l| vec![l.hf_hub.clone()],
        tool: None,
    },
];

pub fn recommend(index: &Index, context: &Context, guard: &Guard, claims: &mut Claims) -> Vec<Recommendation> {
    let mut recs = Vec::new();
    for cache in CACHES {
        let folders: Vec<_> = (cache.folders)(&context.locations)
            .iter()
            .filter_map(|path| find_dir(index, path))
            .filter(|&folder| !claims.covers(index, folder) && guard.allows(index, folder))
            .collect();
        if folders.is_empty() {
            continue;
        }
        let rec = Recommendation::new(format!("cache:{}", cache.id), Rule::PackageCache, cache.tier, cache.title, cache.reason);
        match cache.tool.filter(|(program, _)| context.tools.has(program)) {
            Some((program, args)) => {
                let items: Vec<Item> = folders.iter().map(|&folder| item_of(index, folder)).collect();
                folders.iter().for_each(|&folder| claims.claim(folder));
                let tool = Tool::new(program, args, None);
                let note = format!("{} deletes what it removes outright: it doesn't go to the Recycle Bin.", tool.command);
                recs.push(rec.with_items(items).with_tool(tool).with_note(note));
            }
            None => {
                let mut items = Vec::new();
                for folder in folders {
                    for entry in index.children(folder).collect::<Vec<_>>() {
                        if guard.allows(index, entry) && !claims.covers(index, entry) {
                            claims.claim(entry);
                            items.push(item_of(index, entry));
                        }
                    }
                }
                if !items.is_empty() {
                    recs.push(rec.with_items(items).with_note("Anything a program still has open is skipped."));
                }
            }
        }
    }
    recs
}

#[cfg(test)]
mod tests {
    use crate::testing::{Sandbox, item_paths};
    use crate::{Action, Tier, Tools, scan};

    fn caches(sandbox: &Sandbox) {
        let profile = r"Users\me";
        let local = r"Users\me\AppData\Local";
        for (rel, seed) in [
            (format!(r"{local}\pnpm\store\v10\files\00\abc"), 1),
            (format!(r"{local}\npm-cache\_cacache\content-v2\sha512\aa"), 2),
            (format!(r"{profile}\.cargo\registry\cache\index.crates.io-1\serde-1.0.0.crate"), 3),
            (format!(r"{profile}\.cargo\registry\index\index.crates.io-1\config.json"), 4),
            (format!(r"{profile}\.cargo\git\checkouts\tokio-abc\main\Cargo.toml"), 5),
            (format!(r"{profile}\.cargo\bin\cargo.exe"), 6),
            (format!(r"{local}\pip\cache\http-v2\a\b"), 7),
            (format!(r"{profile}\.nuget\packages\newtonsoft.json\13.0.1\lib.dll"), 8),
            (format!(r"{local}\NuGet\v3-cache\x\list.json"), 9),
            (format!(r"{profile}\.gradle\caches\modules-2\files-2.1\x.jar"), 10),
            (format!(r"{profile}\.gradle\wrapper\dists\gradle-8\gradle.zip"), 11),
            (format!(r"{local}\go-build\00\0011-a"), 12),
            (format!(r"{local}\Yarn\Cache\v6\npm-left-pad.tgz"), 13),
            (format!(r"{profile}\.cache\huggingface\hub\models--org--model\blobs\abc"), 14),
        ] {
            sandbox.write(&rel, 9000, seed);
        }
    }

    #[test]
    fn package_caches_go_to_the_recycle_bin_without_their_tools() {
        let sandbox = Sandbox::new("caches");
        caches(&sandbox);
        let recs = scan(&sandbox.index(), &sandbox.context());
        let paths = item_paths(&sandbox, &recs);
        assert_eq!(
            paths,
            [
                "users/me/.cache/huggingface/hub/models--org--model",
                "users/me/.cargo/git/checkouts/tokio-abc",
                "users/me/.cargo/registry/cache/index.crates.io-1",
                "users/me/.gradle/caches/modules-2",
                "users/me/.nuget/packages/newtonsoft.json",
                "users/me/appdata/local/go-build/00",
                "users/me/appdata/local/npm-cache/_cacache",
                "users/me/appdata/local/nuget/v3-cache/x",
                "users/me/appdata/local/pip/cache/http-v2",
                "users/me/appdata/local/pnpm/store/v10",
                "users/me/appdata/local/yarn/cache/v6",
            ]
        );
        let hf = recs.iter().find(|r| r.id == "cache:huggingface").unwrap();
        assert_eq!(hf.tier, Tier::Review, "models are big downloads");
        assert!(recs.iter().filter(|r| r.id.starts_with("cache:") && r.id != "cache:huggingface").all(|r| r.tier == Tier::Safe));
        assert!(recs.iter().all(|r| !matches!(r.action, Action::Tool(_))));
    }

    #[test]
    fn package_managers_clean_their_own_caches_when_installed() {
        let sandbox = Sandbox::new("caches-tools");
        caches(&sandbox);
        let mut context = sandbox.context();
        context.tools = Tools::with(&["pnpm", "pip", "dotnet", "go"]);
        let recs = scan(&sandbox.index(), &context);
        let command = |id: &str| match &recs.iter().find(|r| r.id == id).unwrap().action {
            Action::Tool(tool) => tool.command.clone(),
            other => format!("{other:?}"),
        };
        assert_eq!(command("cache:pnpm"), "pnpm store prune");
        assert_eq!(command("cache:pip"), "pip cache purge");
        assert_eq!(command("cache:nuget"), "dotnet nuget locals all --clear");
        assert_eq!(command("cache:go"), "go clean -cache");
        assert_eq!(command("cache:npm"), "Recycle");
        let nuget = recs.iter().find(|r| r.id == "cache:nuget").unwrap();
        assert_eq!(nuget.items.len(), 2, "the folders it clears, shown");
        assert!(nuget.note.as_deref().unwrap().contains("doesn't go to the Recycle Bin"));
    }
}
