//! Where the rules look, and what they never touch. Every location is a plain path, so tests
//! (and the app's development hook) point all of them into a scratch folder laid out like a real
//! machine with [`Locations::under`]; only [`Locations::current`] reads the real environment.

use std::path::Path;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Locations {
    /// The owner's profile folder, `C:\Users\me`.
    pub profile: String,
    /// `%LOCALAPPDATA%`.
    pub local: String,
    /// `%APPDATA%`.
    pub roaming: String,
    /// `%TEMP%`.
    pub temp: String,
    pub downloads: String,
    pub documents: String,
    pub pictures: String,
    pub videos: String,
    pub music: String,
    pub desktop: String,
    /// Every OneDrive folder (personal and work).
    pub onedrive: Vec<String>,
    /// `C:\Windows`.
    pub windows: String,
    /// `C:\Program Files` and `C:\Program Files (x86)`.
    pub program_files: Vec<String>,
    pub program_data: String,
    /// The system drive's root, `C:\`, where previous Windows installations are kept.
    pub system_drive: String,
    /// Cargo's home, `%USERPROFILE%\.cargo` unless `CARGO_HOME` says otherwise.
    pub cargo_home: String,
    /// Gradle's home, `%USERPROFILE%\.gradle` unless `GRADLE_USER_HOME` says otherwise.
    pub gradle_home: String,
    /// NuGet's global packages folder.
    pub nuget_packages: String,
    pub pip_cache: String,
    pub npm_cache: String,
    pub go_cache: String,
    /// The Hugging Face hub cache.
    pub hf_hub: String,
}

/// A path joined from a folder and relative parts with backslashes.
pub(crate) fn join(folder: &str, rest: &str) -> String {
    format!("{}\\{}", folder.trim_end_matches('\\'), rest.trim_start_matches('\\'))
}

impl Locations {
    /// A machine laid out under `base`: `base\Users\me` is the profile, `base\Windows` the
    /// system folder, and so on, each where Windows puts it by default.
    pub fn under(base: &Path) -> Self {
        let base = base.to_string_lossy().trim_end_matches('\\').to_owned();
        let profile = join(&base, r"Users\me");
        let local = join(&profile, r"AppData\Local");
        Locations {
            roaming: join(&profile, r"AppData\Roaming"),
            temp: join(&local, "Temp"),
            downloads: join(&profile, "Downloads"),
            documents: join(&profile, "Documents"),
            pictures: join(&profile, "Pictures"),
            videos: join(&profile, "Videos"),
            music: join(&profile, "Music"),
            desktop: join(&profile, "Desktop"),
            onedrive: vec![join(&profile, "OneDrive")],
            windows: join(&base, "Windows"),
            program_files: vec![join(&base, "Program Files"), join(&base, "Program Files (x86)")],
            program_data: join(&base, "ProgramData"),
            system_drive: base.clone(),
            cargo_home: join(&profile, ".cargo"),
            gradle_home: join(&profile, ".gradle"),
            nuget_packages: join(&profile, r".nuget\packages"),
            pip_cache: join(&local, r"pip\cache"),
            npm_cache: join(&local, "npm-cache"),
            go_cache: join(&local, "go-build"),
            hf_hub: join(&profile, r".cache\huggingface\hub"),
            local,
            profile,
        }
    }

    /// This machine's locations, from the known folders and the environment.
    #[cfg(windows)]
    pub fn current() -> Self {
        use crate::system::{known_folder, long_path};
        use windows_sys::Win32::UI::Shell::{
            FOLDERID_Desktop, FOLDERID_Documents, FOLDERID_Downloads, FOLDERID_LocalAppData, FOLDERID_Music,
            FOLDERID_Pictures, FOLDERID_Profile, FOLDERID_RoamingAppData, FOLDERID_Videos,
        };
        let env = |name: &str| std::env::var(name).ok().filter(|v| !v.trim().is_empty()).map(|v| long_path(&v));
        let profile = known_folder(&FOLDERID_Profile).or_else(|| env("USERPROFILE")).unwrap_or_default();
        let local = known_folder(&FOLDERID_LocalAppData).or_else(|| env("LOCALAPPDATA")).unwrap_or_else(|| join(&profile, r"AppData\Local"));
        let roaming =
            known_folder(&FOLDERID_RoamingAppData).or_else(|| env("APPDATA")).unwrap_or_else(|| join(&profile, r"AppData\Roaming"));
        let known = |id, fallback: &str| known_folder(id).unwrap_or_else(|| join(&profile, fallback));
        let windows = env("SystemRoot").or_else(|| env("windir")).unwrap_or_else(|| r"C:\Windows".into());
        let system_drive = env("SystemDrive").map(|d| format!("{}\\", d.trim_end_matches('\\'))).unwrap_or_else(|| r"C:\".into());
        let mut program_files: Vec<String> =
            ["ProgramFiles", "ProgramFiles(x86)", "ProgramW6432"].iter().filter_map(|name| env(name)).collect();
        program_files.dedup_by(|a, b| a.eq_ignore_ascii_case(b));
        if program_files.is_empty() {
            program_files = vec![r"C:\Program Files".into(), r"C:\Program Files (x86)".into()];
        }
        // OneDrive's folders: from its environment variables, and any profile folder named for it
        // ("OneDrive - Contoso" for a work account).
        let mut onedrive: Vec<String> = ["OneDrive", "OneDriveConsumer", "OneDriveCommercial"].iter().filter_map(|name| env(name)).collect();
        if let Ok(entries) = std::fs::read_dir(&profile) {
            for entry in entries.flatten() {
                let name = entry.file_name().to_string_lossy().into_owned();
                if name.to_lowercase().starts_with("onedrive") {
                    onedrive.push(join(&profile, &name));
                }
            }
        }
        onedrive.sort_by_key(|path| path.to_lowercase());
        onedrive.dedup_by(|a, b| a.eq_ignore_ascii_case(b));
        let hf_hub = env("HF_HUB_CACHE")
            .or_else(|| env("HF_HOME").map(|home| join(&home, "hub")))
            .unwrap_or_else(|| join(&profile, r".cache\huggingface\hub"));
        Locations {
            temp: long_path(std::env::temp_dir().to_string_lossy().trim_end_matches('\\')),
            downloads: known(&FOLDERID_Downloads, "Downloads"),
            documents: known(&FOLDERID_Documents, "Documents"),
            pictures: known(&FOLDERID_Pictures, "Pictures"),
            videos: known(&FOLDERID_Videos, "Videos"),
            music: known(&FOLDERID_Music, "Music"),
            desktop: known(&FOLDERID_Desktop, "Desktop"),
            onedrive,
            program_data: env("ProgramData").unwrap_or_else(|| r"C:\ProgramData".into()),
            windows,
            program_files,
            system_drive,
            cargo_home: env("CARGO_HOME").unwrap_or_else(|| join(&profile, ".cargo")),
            gradle_home: env("GRADLE_USER_HOME").unwrap_or_else(|| join(&profile, ".gradle")),
            nuget_packages: env("NUGET_PACKAGES").unwrap_or_else(|| join(&profile, r".nuget\packages")),
            pip_cache: env("PIP_CACHE_DIR").unwrap_or_else(|| join(&local, r"pip\cache")),
            npm_cache: env("npm_config_cache").unwrap_or_else(|| join(&local, "npm-cache")),
            go_cache: env("GOCACHE").unwrap_or_else(|| join(&local, "go-build")),
            hf_hub,
            local,
            roaming,
            profile,
        }
    }

    /// The folders nothing is ever recommended inside: the system's, the owner's own and
    /// OneDrive's. The owner's folders count both where Windows says they are and where they
    /// were before: when OneDrive backs up Documents, Windows points it at `OneDrive\Documents`,
    /// and the old `Documents` in the profile, often still full, is the owner's all the same.
    pub fn protected(&self) -> Vec<String> {
        let mut protected = vec![
            self.windows.clone(),
            self.program_data.clone(),
            self.documents.clone(),
            self.pictures.clone(),
            self.videos.clone(),
            self.music.clone(),
            self.desktop.clone(),
        ];
        if !self.profile.trim().is_empty() {
            protected.extend(["Documents", "Pictures", "Videos", "Music", "Desktop"].iter().map(|name| join(&self.profile, name)));
        }
        protected.extend(self.program_files.iter().cloned());
        protected.extend(self.onedrive.iter().cloned());
        protected.retain(|path| !path.trim().is_empty());
        protected.sort_by_key(|path| path.to_lowercase());
        protected.dedup_by(|a, b| a.eq_ignore_ascii_case(b));
        protected
    }

    /// Where the duplicate finder looks by default: the profile, without AppData.
    pub fn duplicate_roots(&self) -> (Vec<String>, Vec<String>) {
        (vec![self.profile.clone()], vec![join(&self.profile, "AppData")])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_test_machine_is_laid_out_like_a_real_one() {
        let places = Locations::under(Path::new(r"C:\Temp\box\"));
        assert_eq!(places.profile, r"C:\Temp\box\Users\me");
        assert_eq!(places.temp, r"C:\Temp\box\Users\me\AppData\Local\Temp");
        assert_eq!(places.cargo_home, r"C:\Temp\box\Users\me\.cargo");
        assert_eq!(places.system_drive, r"C:\Temp\box");
        let protected = places.protected();
        for folder in ["Windows", r"Users\me\Documents", r"Users\me\OneDrive", "Program Files", "ProgramData"] {
            assert!(protected.contains(&join(r"C:\Temp\box", folder)), "{folder}");
        }
        assert!(!protected.contains(&places.downloads));
    }

    #[test]
    fn owners_folders_moved_into_onedrive_stay_protected_where_they_were() {
        let mut places = Locations::under(Path::new(r"C:\box"));
        places.documents = r"C:\box\Users\me\OneDrive\Documents".into();
        places.pictures = r"C:\box\Users\me\OneDrive\Pictures".into();
        let protected = places.protected();
        for folder in [r"C:\box\Users\me\OneDrive\Documents", r"C:\box\Users\me\Documents", r"C:\box\Users\me\Pictures"] {
            assert!(protected.iter().any(|p| p == folder), "{folder} in {protected:?}");
        }
    }

    #[cfg(windows)]
    #[test]
    fn the_real_locations_are_found() {
        let places = Locations::current();
        assert!(Path::new(&places.profile).is_dir());
        assert!(Path::new(&places.temp).is_dir());
        assert!(!places.program_files.is_empty());
        assert!(!places.temp.contains('~'), "long names, as the index holds them: {}", places.temp);
    }
}
