//! Running under the Windows service manager.

use std::ffi::OsString;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use anyhow::{Context, Result};
use windows_service::service::{ServiceControl, ServiceControlAccept, ServiceExitCode, ServiceState, ServiceStatus, ServiceType};
use windows_service::service_control_handler::{self, ServiceControlHandlerResult};
use windows_service::{define_windows_service, service_dispatcher};

use crate::{SERVICE_NAME, allowed_account, pipe};

define_windows_service!(ffi_service_main, service_main);

/// Hands control to the service manager, which calls `service_main`.
pub fn run() -> Result<()> {
    service_dispatcher::start(SERVICE_NAME, ffi_service_main).context("this mode is for the service manager; use `install` instead")
}

fn service_main(_arguments: Vec<OsString>) {
    let _ = serve_as_service();
}

fn serve_as_service() -> Result<()> {
    // The account to serve is fixed at install time, in the service's command line.
    let args: Vec<String> = std::env::args().collect();
    let allowed = allowed_account(&args)?.context("the service was installed without an account")?;
    let stopping = Arc::new(AtomicBool::new(false));

    let flag = stopping.clone();
    let status = service_control_handler::register(SERVICE_NAME, move |control| match control {
        ServiceControl::Stop | ServiceControl::Shutdown => {
            flag.store(true, Ordering::SeqCst);
            pipe::wake();
            ServiceControlHandlerResult::NoError
        }
        ServiceControl::Interrogate => ServiceControlHandlerResult::NoError,
        _ => ServiceControlHandlerResult::NotImplemented,
    })?;
    let report = |state, accept| {
        status.set_service_status(ServiceStatus {
            service_type: ServiceType::OWN_PROCESS,
            current_state: state,
            controls_accepted: accept,
            exit_code: ServiceExitCode::Win32(0),
            checkpoint: 0,
            wait_hint: Duration::from_secs(5),
            process_id: None,
        })
    };
    report(ServiceState::Running, ServiceControlAccept::STOP | ServiceControlAccept::SHUTDOWN)?;
    let served = pipe::serve(&allowed, &stopping);
    report(ServiceState::Stopped, ServiceControlAccept::empty())?;
    served
}
