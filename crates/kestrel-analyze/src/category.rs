//! What type of data a file is: by where it lives first (everything under `C:\Windows` is the
//! system's, everything in a `node_modules` folder is build output), then by its extension.
//!
//! Locations are worked out top down as a walk descends, one folder at a time ([`Place`]), so
//! classifying a file costs a table lookup by its extension id.

use kestrel_index::{EntryId, Index, NO_ENTRY};
use serde::{Deserialize, Serialize};

/// The types the disk usage breakdown sorts files into.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Category {
    Video,
    Images,
    Audio,
    Documents,
    Archives,
    Code,
    Programs,
    DiskImages,
    BuildOutput,
    System,
    Other,
}

impl Category {
    pub const ALL: [Category; 11] = [
        Category::Video,
        Category::Images,
        Category::Audio,
        Category::Documents,
        Category::Archives,
        Category::Code,
        Category::Programs,
        Category::DiskImages,
        Category::BuildOutput,
        Category::System,
        Category::Other,
    ];

    pub fn index(self) -> usize {
        self as usize
    }

    pub fn label(self) -> &'static str {
        match self {
            Category::Video => "Video",
            Category::Images => "Images",
            Category::Audio => "Audio",
            Category::Documents => "Documents",
            Category::Archives => "Archives",
            Category::Code => "Code",
            Category::Programs => "Programs and libraries",
            Category::DiskImages => "Disk images and virtual machines",
            Category::BuildOutput => "Developer build output",
            Category::System => "System",
            Category::Other => "Other",
        }
    }
}

/// The category of a file by its extension (lower case, without the dot), if it has one.
pub fn by_extension(ext: &str) -> Option<Category> {
    use Category::*;
    Some(match ext {
        "mp4" | "m4v" | "mkv" | "mov" | "avi" | "wmv" | "webm" | "mpg" | "mpeg" | "m2ts" | "mts" | "vob" | "flv" | "f4v"
        | "3gp" | "3g2" | "ogv" | "rm" | "rmvb" | "asf" | "divx" | "braw" | "r3d" | "mxf" => Video,
        "jpg" | "jpeg" | "jpe" | "jfif" | "png" | "gif" | "bmp" | "dib" | "tif" | "tiff" | "webp" | "heic" | "heif" | "avif"
        | "jxl" | "ico" | "svg" | "svgz" | "psd" | "psb" | "ai" | "eps" | "xcf" | "kra" | "raw" | "cr2" | "cr3" | "crw"
        | "nef" | "nrw" | "arw" | "srf" | "sr2" | "dng" | "orf" | "rw2" | "raf" | "pef" | "srw" | "x3f" | "exr" | "hdr"
        | "tga" | "dds" => Images,
        "mp3" | "wav" | "flac" | "aac" | "m4a" | "m4b" | "ogg" | "oga" | "opus" | "wma" | "aif" | "aiff" | "aifc" | "alac"
        | "ape" | "mid" | "midi" | "mka" | "ac3" | "dts" | "amr" | "wv" | "caf" => Audio,
        "pdf" | "doc" | "docx" | "docm" | "dot" | "dotx" | "odt" | "ott" | "rtf" | "txt" | "md" | "markdown" | "tex" | "xls"
        | "xlsx" | "xlsm" | "xlsb" | "ods" | "csv" | "tsv" | "ppt" | "pptx" | "pptm" | "odp" | "key" | "pages" | "numbers"
        | "epub" | "mobi" | "azw" | "azw3" | "djvu" | "xps" | "oxps" | "one" | "onepkg" | "msg" | "eml" | "pst" | "ost"
        | "vsd" | "vsdx" | "pub" => Documents,
        "zip" | "7z" | "rar" | "tar" | "gz" | "tgz" | "bz2" | "tbz2" | "xz" | "txz" | "zst" | "tzst" | "lz" | "lzma"
        | "lz4" | "cab" | "arj" | "z" | "ace" | "nupkg" | "whl" => Archives,
        // `.mts` is taken by video above: camcorder recordings far outweigh TypeScript modules.
        "rs" | "ts" | "tsx" | "cts" | "js" | "jsx" | "mjs" | "cjs" | "py" | "pyi" | "go" | "c" | "h" | "cc" | "cpp"
        | "cxx" | "hpp" | "hh" | "hxx" | "inl" | "cs" | "fs" | "fsx" | "vb" | "java" | "kt" | "kts" | "scala" | "groovy"
        | "php" | "rb" | "swift" | "m" | "mm" | "dart" | "lua" | "sh" | "bash" | "zsh" | "ps1" | "psm1" | "psd1" | "bat"
        | "cmd" | "html" | "htm" | "css" | "scss" | "sass" | "less" | "vue" | "svelte" | "zig" | "hs" | "ex" | "exs"
        | "erl" | "clj" | "cljs" | "r" | "jl" | "sql" | "hlsl" | "glsl" | "shader" | "wgsl" | "gd" | "nim" | "ml"
        | "mli" | "pl" | "pm" | "asm" | "cu" | "json" | "jsonc" | "json5" | "toml" | "yaml" | "yml" | "xml" | "ipynb"
        | "sln" | "csproj" | "fsproj" | "vbproj" | "vcxproj" | "props" | "targets" | "cmake" | "gradle" | "lock"
        | "map" => Code,
        "exe" | "dll" | "ocx" | "cpl" | "scr" | "msi" | "msix" | "msixbundle" | "appx" | "appxbundle" | "so" | "dylib"
        | "lib" | "a" | "pdb" | "jar" | "war" | "apk" | "deb" | "rpm" | "pyd" | "node" | "winmd" | "mui" | "efi" | "com"
        | "wasm" | "ax" | "xll" => Programs,
        "iso" | "img" | "vhd" | "vhdx" | "avhd" | "avhdx" | "vmdk" | "vdi" | "qcow" | "qcow2" | "ova" | "ovf" | "vmem"
        | "vmsn" | "vmss" | "nvram" | "wim" | "esd" | "swm" | "dmg" | "vfd" | "ffu" => DiskImages,
        "o" | "pch" | "ipch" | "ilk" | "idb" | "tlog" | "pyc" | "pyo" | "class" | "rlib" | "rmeta" => BuildOutput,
        "sys" | "drv" | "cat" | "mum" | "evtx" | "etl" | "dmp" | "mdmp" => System,
        _ => return None,
    })
}

/// What a folder's location says about everything beneath it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Place {
    /// Nothing: files are known by their extensions.
    #[default]
    Plain,
    /// Windows itself, its recovery and update folders, restore points.
    System,
    /// Installed programs.
    Programs,
    /// A build folder or a package cache that a developer tool can make again.
    BuildOutput,
}

impl Place {
    pub fn category(self) -> Option<Category> {
        match self {
            Place::Plain => None,
            Place::System => Some(Category::System),
            Place::Programs => Some(Category::Programs),
            Place::BuildOutput => Some(Category::BuildOutput),
        }
    }
}

/// Folders directly in a drive's root that belong to Windows.
const SYSTEM_ROOT_FOLDERS: &[&str] = &[
    "windows",
    "windows.old",
    "$windows.~bt",
    "$windows.~ws",
    "$winreagent",
    "$sysreset",
    "$getcurrent",
    "recovery",
    "system volume information",
    "config.msi",
    "boot",
];

/// Files directly in a drive's root that belong to Windows: the page file, the hibernation
/// file and the swap file for apps, and crash dump scratch.
pub const SYSTEM_ROOT_FILES: &[&str] = &["pagefile.sys", "hiberfil.sys", "swapfile.sys", "dumpstack.log.tmp", "dumpstack.log"];

/// Folders that are build output or dependencies wherever they are.
const BUILD_FOLDERS: &[&str] = &[
    "node_modules",
    ".venv",
    "__pycache__",
    ".next",
    ".nuxt",
    ".turbo",
    ".parcel-cache",
    ".svelte-kit",
    ".angular",
    ".gradle",
    "cmakefiles",
    ".pytest_cache",
    ".mypy_cache",
    ".tox",
];

/// Project files whose siblings named `bin` or `obj` are .NET or C++ build output.
const DOTNET_PROJECT_EXTS: &[&str] = &["csproj", "fsproj", "vbproj", "vcxproj", "sln"];

/// Classifies files and folders of one index. Made afresh for each analysis, while the index is
/// held, since the extension table grows as files with new extensions appear.
pub struct Classifier {
    /// The category of each extension id.
    exts: Vec<Option<Category>>,
    dotnet_exts: Vec<u16>,
    uproject_ext: Option<u16>,
}

impl Classifier {
    pub fn new(index: &Index) -> Self {
        let exts = (0..index.ext_count()).map(|ext| by_extension(index.ext_name(ext as u16))).collect();
        Self {
            exts,
            dotnet_exts: DOTNET_PROJECT_EXTS.iter().filter_map(|ext| index.ext_id(ext)).collect(),
            uproject_ext: index.ext_id("uproject"),
        }
    }

    /// The category of the file `id`, which lies in a folder at `place`.
    pub fn file(&self, index: &Index, id: EntryId, place: Place) -> Category {
        if let Some(category) = place.category() {
            return category;
        }
        if let Some(category) = self.exts.get(index.ext(id) as usize).copied().flatten() {
            return category;
        }
        let parent = index.parent(id);
        if parent != NO_ENTRY && is_drive_root(index, parent) {
            let name = index.lower_bytes(id);
            if SYSTEM_ROOT_FILES.iter().any(|system| system.as_bytes() == name) {
                return Category::System;
            }
        }
        Category::Other
    }

    /// The place of the folder `dir`, a child of `parent`, which is at `parent_place`.
    pub fn child_place(&self, index: &Index, parent: EntryId, parent_place: Place, dir: EntryId) -> Place {
        if parent_place != Place::Plain {
            return parent_place;
        }
        let Ok(name) = std::str::from_utf8(index.lower_bytes(dir)) else { return Place::Plain };
        if is_drive_root(index, parent) {
            if SYSTEM_ROOT_FOLDERS.contains(&name) {
                return Place::System;
            }
            if name == "program files" || name == "program files (x86)" {
                return Place::Programs;
            }
        }
        if BUILD_FOLDERS.contains(&name) || name.starts_with("cmake-build-") {
            return Place::BuildOutput;
        }
        let parent_name = index.lower_bytes(parent);
        let grandparent = index.parent(parent);
        let grandparent_name: &[u8] = if grandparent == NO_ENTRY { b"" } else { index.lower_bytes(grandparent) };
        let sibling = |wanted: &str| has_child(index, parent, wanted);
        let built = match name {
            // Programs installed for one account: AppData\Local\Programs.
            "programs" if parent_name == b"local" && grandparent_name == b"appdata" => return Place::Programs,
            "obj" => self.has_child_ext(index, parent, &self.dotnet_exts) || sibling("bin"),
            "bin" => self.has_child_ext(index, parent, &self.dotnet_exts) || sibling("obj"),
            "target" => sibling("cargo.toml") || sibling("pom.xml"),
            "dist" | "out" => sibling("package.json"),
            "build" => ["package.json", "cmakelists.txt", "build.gradle", "build.gradle.kts", "setup.py", "pyproject.toml"]
                .iter()
                .any(|marker| sibling(marker)),
            "venv" | "env" => has_child(index, dir, "pyvenv.cfg"),
            "intermediate" | "deriveddatacache" | "binaries" => {
                self.uproject_ext.is_some_and(|ext| self.has_child_ext(index, parent, &[ext]))
            }
            "library" => sibling("assets") && sibling("projectsettings"),
            // Package caches, which the tools that made them fill again on demand.
            "registry" | "git" => parent_name == b".cargo",
            "packages" => parent_name == b".nuget",
            "repository" => parent_name == b".m2",
            "npm-cache" => true,
            "store" => parent_name == b"pnpm",
            "cache" => parent_name == b"yarn" || parent_name == b"pip",
            _ => false,
        };
        if built { Place::BuildOutput } else { Place::Plain }
    }

    /// The place of any folder, worked out from the top of its drive down.
    pub fn place_of(&self, index: &Index, dir: EntryId) -> Place {
        let mut chain = Vec::new();
        let mut cur = dir;
        while cur != NO_ENTRY {
            chain.push(cur);
            cur = index.parent(cur);
        }
        let mut place = Place::Plain;
        for pair in chain.windows(2).rev() {
            let (child, parent) = (pair[0], pair[1]);
            place = self.child_place(index, parent, place, child);
        }
        place
    }

    fn has_child_ext(&self, index: &Index, dir: EntryId, exts: &[u16]) -> bool {
        !exts.is_empty() && index.children(dir).any(|child| !index.is_dir(child) && exts.contains(&index.ext(child)))
    }
}

/// Whether `id` is the root of a drive (`C:`), rather than a folder indexed as a root of its own.
pub fn is_drive_root(index: &Index, id: EntryId) -> bool {
    index.parent(id) == NO_ENTRY && index.name_bytes(id).last() == Some(&b':')
}

fn has_child(index: &Index, dir: EntryId, folded_name: &str) -> bool {
    index.children(dir).any(|child| index.lower_bytes(child) == folded_name.as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{F, build, id};

    #[test]
    fn extensions_pick_categories() {
        assert_eq!(by_extension("mkv"), Some(Category::Video));
        assert_eq!(by_extension("heic"), Some(Category::Images));
        assert_eq!(by_extension("flac"), Some(Category::Audio));
        assert_eq!(by_extension("docx"), Some(Category::Documents));
        assert_eq!(by_extension("7z"), Some(Category::Archives));
        assert_eq!(by_extension("rs"), Some(Category::Code));
        assert_eq!(by_extension("dll"), Some(Category::Programs));
        assert_eq!(by_extension("vhdx"), Some(Category::DiskImages));
        assert_eq!(by_extension("pyc"), Some(Category::BuildOutput));
        assert_eq!(by_extension("sys"), Some(Category::System));
        assert_eq!(by_extension("weird"), None);
        // Every category has a label of its own and its place in `ALL`.
        for (n, category) in Category::ALL.iter().enumerate() {
            assert_eq!(category.index(), n);
            assert!(!category.label().is_empty());
        }
    }

    #[test]
    fn locations_override_extensions() {
        let index = build(
            &[
                F(r"Windows\System32\kernel32.dll", 10, 0, 0),
                F(r"Windows\Media\chime.wav", 10, 0, 0),
                F(r"Program Files\App\intro.mp4", 10, 0, 0),
                F(r"Users\me\AppData\Local\Programs\Code\Code.exe", 10, 0, 0),
                F(r"Users\me\web\package.json", 10, 0, 0),
                F(r"Users\me\web\node_modules\left-pad\index.js", 10, 0, 0),
                F(r"Users\me\web\dist\bundle.js", 10, 0, 0),
                F(r"Users\me\rust\Cargo.toml", 10, 0, 0),
                F(r"Users\me\rust\target\debug\app.exe", 10, 0, 0),
                F(r"Users\me\notes\target\goal.txt", 10, 0, 0),
                F(r"Users\me\.cargo\registry\cache\x.crate", 10, 0, 0),
                F(r"Users\me\dotnet\App.csproj", 10, 0, 0),
                F(r"Users\me\dotnet\bin\Debug\App.dll", 10, 0, 0),
                F(r"Users\me\py\venv\pyvenv.cfg", 10, 0, 0),
                F(r"Users\me\Videos\holiday.mp4", 10, 0, 0),
                F(r"Users\me\thing.unknown", 10, 0, 0),
                F("pagefile.sys", 10, 0, 0),
                F("DumpStack.log.tmp", 10, 0, 0),
                F(r"Users\me\DumpStack.log.tmp", 10, 0, 0),
            ],
            &[],
        );
        let classifier = Classifier::new(&index);
        let category = |path: &str| {
            let file = id(&index, path);
            classifier.file(&index, file, classifier.place_of(&index, index.parent(file)))
        };
        assert_eq!(category(r"C:\Windows\System32\kernel32.dll"), Category::System);
        assert_eq!(category(r"C:\Windows\Media\chime.wav"), Category::System);
        assert_eq!(category(r"C:\Program Files\App\intro.mp4"), Category::Programs);
        assert_eq!(category(r"C:\Users\me\AppData\Local\Programs\Code\Code.exe"), Category::Programs);
        assert_eq!(category(r"C:\Users\me\web\package.json"), Category::Code);
        assert_eq!(category(r"C:\Users\me\web\node_modules\left-pad\index.js"), Category::BuildOutput);
        assert_eq!(category(r"C:\Users\me\web\dist\bundle.js"), Category::BuildOutput);
        assert_eq!(category(r"C:\Users\me\rust\target\debug\app.exe"), Category::BuildOutput);
        // `target` without Cargo.toml or pom.xml beside it is somebody's own folder.
        assert_eq!(category(r"C:\Users\me\notes\target\goal.txt"), Category::Documents);
        assert_eq!(category(r"C:\Users\me\.cargo\registry\cache\x.crate"), Category::BuildOutput);
        assert_eq!(category(r"C:\Users\me\dotnet\bin\Debug\App.dll"), Category::BuildOutput);
        assert_eq!(category(r"C:\Users\me\py\venv\pyvenv.cfg"), Category::BuildOutput);
        assert_eq!(category(r"C:\Users\me\Videos\holiday.mp4"), Category::Video);
        assert_eq!(category(r"C:\Users\me\thing.unknown"), Category::Other);
        assert_eq!(category(r"C:\pagefile.sys"), Category::System);
        // Crash dump scratch is the system's only in the root of the drive.
        assert_eq!(category(r"C:\DumpStack.log.tmp"), Category::System);
        assert_eq!(category(r"C:\Users\me\DumpStack.log.tmp"), Category::Other);
        assert_eq!(classifier.place_of(&index, id(&index, r"C:\Windows")), Place::System);
        assert_eq!(classifier.place_of(&index, id(&index, r"C:\Users")), Place::Plain);
    }
}
