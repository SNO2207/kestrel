//! The Kestrel broker: a Windows service that reads drives' master file tables, which needs
//! rights ordinary programs do not have, and hands the resulting index to the Kestrel app.
//!
//! It serves one named pipe, opened only to the account that installed it (plus SYSTEM and
//! Administrators), and it only ever reads: there is no request that writes anything.
//!
//! Usage, from an administrator terminal:
//!   kestrel-broker install     copy to Program Files, register and start the service
//!     --journal-mb N           also enlarge each fixed NTFS drive's change journal to N MB
//!   kestrel-broker uninstall   stop and remove it
//!   kestrel-broker console     serve in the foreground until Ctrl+C, for testing
//!
//! Enlarging the journals is the one change the install makes to drives, and only when asked;
//! the service itself still only reads.

mod journal;
mod pipe;
mod service;

use std::ffi::OsString;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use windows_service::service::{
    ServiceAccess, ServiceErrorControl, ServiceInfo, ServiceStartType, ServiceState, ServiceType,
};
use windows_service::service_manager::{ServiceManager, ServiceManagerAccess};

pub const SERVICE_NAME: &str = "KestrelBroker";
const DISPLAY_NAME: &str = "Kestrel index broker";
const DESCRIPTION: &str = "Reads drives' file tables so the Kestrel file explorer can index them in seconds. \
     Only the account that installed it can connect, and it never changes anything on disk.";

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    match args.get(1).map(String::as_str) {
        Some("install") => install(&args),
        Some("uninstall") => uninstall(),
        Some("service") => service::run(),
        Some("console") => {
            let allowed = allowed_account(&args)?.unwrap_or(pipe::current_user_sid()?);
            println!("serving {} for {allowed}; press Ctrl+C to stop", kestrel_index::broker::PIPE);
            pipe::serve(&allowed, &Default::default())
        }
        _ => {
            println!("usage: kestrel-broker install [--journal-mb N] | uninstall | console");
            Ok(())
        }
    }
}

/// The value after `--allow`, if given.
pub fn allowed_account(args: &[String]) -> Result<Option<String>> {
    match args.iter().position(|a| a == "--allow") {
        None => Ok(None),
        Some(at) => args.get(at + 1).cloned().map(Some).context("--allow needs an account SID"),
    }
}

/// Where the installed copy lives. Program Files is writable only by administrators, which
/// matters: a service's binary in a folder its user can write to would let any program that
/// user runs replace it and gain SYSTEM rights.
fn installed_path() -> PathBuf {
    let base = std::env::var_os("ProgramFiles").map(PathBuf::from).unwrap_or_else(|| PathBuf::from(r"C:\Program Files"));
    base.join("Kestrel").join("kestrel-broker.exe")
}

fn install(args: &[String]) -> Result<()> {
    // Read before anything changes, so a mistyped option changes nothing.
    let journal_mb = journal::requested_mb(args)?;
    let user = pipe::current_user_sid().context("finding the current account")?;
    let manager = ServiceManager::local_computer(None::<&str>, ServiceManagerAccess::CONNECT | ServiceManagerAccess::CREATE_SERVICE)
        .context("opening the service manager; run this from an administrator terminal")?;

    // A previous installation is stopped first, so its binary can be replaced.
    if let Ok(existing) = manager.open_service(SERVICE_NAME, ServiceAccess::STOP | ServiceAccess::QUERY_STATUS) {
        stop(&existing)?;
    }
    let target = installed_path();
    std::fs::create_dir_all(target.parent().unwrap())?;
    std::fs::copy(std::env::current_exe()?, &target).with_context(|| format!("copying to {}", target.display()))?;

    let info = ServiceInfo {
        name: OsString::from(SERVICE_NAME),
        display_name: OsString::from(DISPLAY_NAME),
        service_type: ServiceType::OWN_PROCESS,
        start_type: ServiceStartType::AutoStart,
        error_control: ServiceErrorControl::Normal,
        executable_path: target.clone(),
        launch_arguments: vec![OsString::from("service"), OsString::from("--allow"), OsString::from(&user)],
        dependencies: vec![],
        // LocalSystem.
        account_name: None,
        account_password: None,
    };
    let access = ServiceAccess::CHANGE_CONFIG | ServiceAccess::START | ServiceAccess::QUERY_STATUS;
    let service = match manager.open_service(SERVICE_NAME, access) {
        Ok(service) => {
            service.change_config(&info)?;
            service
        }
        Err(_) => manager.create_service(&info, access).context("registering the service")?,
    };
    service.set_description(DESCRIPTION)?;
    service.start::<&str>(&[]).context("starting the service")?;
    println!("installed {} as the {SERVICE_NAME} service, for account {user}", target.display());
    if let Some(mb) = journal_mb {
        journal::enlarge_all(mb);
    }
    Ok(())
}

fn uninstall() -> Result<()> {
    let manager = ServiceManager::local_computer(None::<&str>, ServiceManagerAccess::CONNECT)
        .context("opening the service manager; run this from an administrator terminal")?;
    let service = manager
        .open_service(SERVICE_NAME, ServiceAccess::STOP | ServiceAccess::QUERY_STATUS | ServiceAccess::DELETE)
        .context("the broker is not installed")?;
    stop(&service)?;
    service.delete()?;
    let _ = std::fs::remove_file(installed_path());
    println!("removed the {SERVICE_NAME} service");
    Ok(())
}

fn stop(service: &windows_service::service::Service) -> Result<()> {
    if service.query_status()?.current_state != ServiceState::Stopped {
        let _ = service.stop();
        let deadline = Instant::now() + Duration::from_secs(20);
        while service.query_status()?.current_state != ServiceState::Stopped {
            if Instant::now() > deadline {
                bail!("the service did not stop");
            }
            std::thread::sleep(Duration::from_millis(200));
        }
    }
    Ok(())
}
