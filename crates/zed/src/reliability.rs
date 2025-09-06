use crate::stdout_is_a_pty;
use backtrace::{self, Backtrace};
use chrono::Utc;
use client::telemetry::{self, MINIDUMP_ENDPOINT};
use gpui::SemanticVersion;
use release_channel::{AppCommitSha, RELEASE_CHANNEL, ReleaseChannel};
use std::{
    env,
    ffi::c_void,
    fs,
    io::Write,
    panic,
    sync::atomic::{AtomicU32, Ordering},
    thread,
};
use telemetry_events::LocationData;
use util::ResultExt;

static PANIC_COUNT: AtomicU32 = AtomicU32::new(0);

pub fn init_panic_hook(
    app_version: SemanticVersion,
    app_commit_sha: Option<AppCommitSha>,
    system_id: Option<String>,
    installation_id: Option<String>,
    session_id: String,
) {
    let is_pty = stdout_is_a_pty();

    panic::set_hook(Box::new(move |info| {
        let prior_panic_count = PANIC_COUNT.fetch_add(1, Ordering::SeqCst);
        if prior_panic_count > 0 {
            // Give the panic-ing thread time to write the panic file
            loop {
                thread::yield_now();
            }
        }

        let payload = info
            .payload()
            .downcast_ref::<&str>()
            .map(|s| s.to_string())
            .or_else(|| info.payload().downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "Box<Any>".to_string());

        if *release_channel::RELEASE_CHANNEL != ReleaseChannel::Dev {
            crashes::handle_panic(payload.clone(), info.location());
        }

        let thread = thread::current();
        let thread_name = thread.name().unwrap_or("<unnamed>");

        if *release_channel::RELEASE_CHANNEL == ReleaseChannel::Dev {
            let location = info.location().unwrap();
            let backtrace = Backtrace::new();
            eprintln!(
                "Thread {:?} panicked with {:?} at {}:{}:{}\n{}{:?}",
                thread_name,
                payload,
                location.file(),
                location.line(),
                location.column(),
                match app_commit_sha.as_ref() {
                    Some(commit_sha) => format!(
                        "https://github.com/zed-industries/zed/blob/{}/{}#L{} \
                        (may not be uploaded, line may be incorrect if files modified)\n",
                        commit_sha.full(),
                        location.file(),
                        location.line()
                    ),
                    None => "".to_string(),
                },
                backtrace,
            );
            if MINIDUMP_ENDPOINT.is_none() {
                std::process::exit(-1);
            }
        }
        let main_module_base_address = get_main_module_base_address();

        let backtrace = Backtrace::new();
        let mut symbols = backtrace
            .frames()
            .iter()
            .flat_map(|frame| {
                let base = frame
                    .module_base_address()
                    .unwrap_or(main_module_base_address);
                frame.symbols().iter().map(move |symbol| {
                    format!(
                        "{}+{}",
                        symbol
                            .name()
                            .as_ref()
                            .map_or("<unknown>".to_owned(), <_>::to_string),
                        (frame.ip() as isize).saturating_sub(base as isize)
                    )
                })
            })
            .collect::<Vec<_>>();

        // Strip out leading stack frames for rust panic-handling.
        if let Some(ix) = symbols
            .iter()
            .position(|name| name == "rust_begin_unwind" || name == "_rust_begin_unwind")
        {
            symbols.drain(0..=ix);
        }

        let panic_data = telemetry_events::Panic {
            thread: thread_name.into(),
            payload,
            location_data: info.location().map(|location| LocationData {
                file: location.file().into(),
                line: location.line(),
            }),
            app_version: app_version.to_string(),
            app_commit_sha: app_commit_sha.as_ref().map(|sha| sha.full()),
            release_channel: RELEASE_CHANNEL.dev_name().into(),
            target: env!("TARGET").to_owned().into(),
            os_name: telemetry::os_name(),
            os_version: Some(telemetry::os_version()),
            architecture: env::consts::ARCH.into(),
            panicked_on: Utc::now().timestamp_millis(),
            backtrace: symbols,
            system_id: system_id.clone(),
            installation_id: installation_id.clone(),
            session_id: session_id.clone(),
        };

        if let Some(panic_data_json) = serde_json::to_string_pretty(&panic_data).log_err() {
            log::error!("{}", panic_data_json);
        }
        zlog::flush();

        if (!is_pty || MINIDUMP_ENDPOINT.is_some())
            && let Some(panic_data_json) = serde_json::to_string(&panic_data).log_err()
        {
            let timestamp = chrono::Utc::now().format("%Y_%m_%d %H_%M_%S").to_string();
            let panic_file_path = paths::logs_dir().join(format!("zed-{timestamp}.panic"));
            let panic_file = fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&panic_file_path)
                .log_err();
            if let Some(mut panic_file) = panic_file {
                writeln!(&mut panic_file, "{panic_data_json}").log_err();
                panic_file.flush().log_err();
            }
        }

        std::process::abort();
    }));
}

fn get_main_module_base_address() -> *mut c_void {
    let mut dl_info = libc::Dl_info {
        dli_fname: std::ptr::null(),
        dli_fbase: std::ptr::null_mut(),
        dli_sname: std::ptr::null(),
        dli_saddr: std::ptr::null_mut(),
    };
    unsafe {
        libc::dladdr(get_main_module_base_address as _, &mut dl_info);
    }
    dl_info.dli_fbase
}
