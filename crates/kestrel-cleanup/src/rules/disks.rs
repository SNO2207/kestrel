//! Disk images and virtual machine disks: WSL's `ext4.vhdx`, Docker's data disk, Hyper-V's and
//! VirtualBox's (and VMware's) disks. Shown only, with how to compact each: deleting one
//! deletes a whole machine, and compacting needs the machine shut down.

use kestrel_index::Index;

use crate::tree::{item_of, with_extensions};
use crate::{Claims, Context, Guard, Item, Recommendation, Rule, Tier, fold_path};

pub const EXTENSIONS: &[&str] = &["vhd", "vhdx", "avhdx", "vmdk", "vdi"];

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Wsl,
    Docker,
    HyperV,
    VirtualBox,
    VMware,
}

fn kind(path: &str) -> Kind {
    let folded = fold_path(path);
    let name = folded.rsplit('\\').next().unwrap_or("");
    if folded.contains(r"\docker\") || name == "docker_data.vhdx" {
        Kind::Docker
    } else if name == "ext4.vhdx" {
        Kind::Wsl
    } else if name.ends_with(".vdi") || folded.contains(r"\virtualbox vms\") {
        Kind::VirtualBox
    } else if name.ends_with(".vmdk") {
        Kind::VMware
    } else {
        Kind::HyperV
    }
}

fn describe(kind: Kind) -> (&'static str, &'static str, &'static str, &'static str) {
    match kind {
        Kind::Wsl => (
            "wsl",
            "WSL disks",
            "Linux distributions under WSL keep their files in these disks. They grow as files are added but don't shrink \
             when files are deleted.",
            "Run `wsl --shutdown`, then `wsl --manage <distribution> --set-sparse true` so the disk gives back free space \
             by itself (WSL 2.0 or later).",
        ),
        Kind::Docker => (
            "docker",
            "Docker's data disk",
            "Docker Desktop keeps images, containers and volumes in this disk, which doesn't shrink by itself.",
            "Remove what you no longer use with `docker system prune` (add `--volumes` for volumes too), or use Docker \
             Desktop's Troubleshoot page to clean up; Docker then compacts the disk.",
        ),
        Kind::HyperV => (
            "hyperv",
            "Hyper-V disks",
            "Virtual machine disks. A dynamic disk grows as the machine writes and doesn't shrink by itself.",
            "Shut the machine down, then compact the disk in Hyper-V Manager (Edit Disk, Compact) or with \
             `Optimize-VHD -Path <disk> -Mode Full`, which needs an administrator.",
        ),
        Kind::VirtualBox => (
            "virtualbox",
            "VirtualBox disks",
            "VirtualBox virtual machine disks, which grow as the machine writes and don't shrink by themselves.",
            "Zero the free space inside the machine, shut it down, then run `VBoxManage modifymedium disk \"<disk>\" --compact`.",
        ),
        Kind::VMware => (
            "vmware",
            "VMware disks",
            "VMware virtual machine disks, which grow as the machine writes and don't shrink by themselves.",
            "Shut the machine down, then use Settings, Hard Disk, Compact in VMware.",
        ),
    }
}

pub fn recommend(index: &Index, _context: &Context, guard: &Guard, claims: &mut Claims) -> Vec<Recommendation> {
    let mut by_kind: Vec<(Kind, Vec<Item>)> = Vec::new();
    for entry in with_extensions(index, EXTENSIONS) {
        if claims.covers(index, entry) || !guard.allows(index, entry) {
            continue;
        }
        claims.claim(entry);
        let item = item_of(index, entry);
        let kind = kind(&item.path);
        match by_kind.iter_mut().find(|(k, _)| *k == kind) {
            Some((_, items)) => items.push(item),
            None => by_kind.push((kind, vec![item])),
        }
    }
    by_kind
        .into_iter()
        .map(|(kind, items)| {
            let (id, title, reason, advice) = describe(kind);
            Recommendation::new(format!("disks:{id}"), Rule::DiskImages, Tier::Info, title, reason)
                .with_items(items)
                .with_advice(advice)
                .with_note("Kestrel never touches these disks.")
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use crate::testing::{Sandbox, item_paths};
    use crate::{Action, Tier, scan};

    #[test]
    fn virtual_disks_are_shown_with_how_to_compact_them_and_never_offered() {
        let sandbox = Sandbox::new("disks");
        let local = r"Users\me\AppData\Local";
        for (rel, seed) in [
            (format!(r"{local}\Packages\CanonicalGroupLimited.Ubuntu_79rhkp1fndgsc\LocalState\ext4.vhdx"), 1),
            (format!(r"{local}\Docker\wsl\disk\docker_data.vhdx"), 2),
            (r"Users\me\VirtualBox VMs\Win\Win.vdi".to_owned(), 3),
            (r"Users\me\vms\server.vhdx".to_owned(), 4),
            (r"ProgramData\Microsoft\Windows\Virtual Hard Disks\protected.vhdx".to_owned(), 5),
        ] {
            sandbox.write(&rel, 9000, seed);
        }
        let recs = scan(&sandbox.index(), &sandbox.context());
        let disks: Vec<_> = recs.iter().filter(|r| r.id.starts_with("disks:")).cloned().collect();
        assert!(disks.iter().all(|r| r.tier == Tier::Info && r.action == Action::None && r.advice.is_some()));
        let ids: Vec<&str> = disks.iter().map(|r| r.id.as_str()).collect();
        for id in ["disks:wsl", "disks:docker", "disks:virtualbox", "disks:hyperv"] {
            assert!(ids.contains(&id), "{ids:?}");
        }
        let wsl = disks.iter().find(|r| r.id == "disks:wsl").unwrap();
        assert!(wsl.advice.as_deref().unwrap().contains("wsl --shutdown"));
        assert!(!item_paths(&sandbox, &recs).iter().any(|p| p.starts_with("programdata")));
    }
}
