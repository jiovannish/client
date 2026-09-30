//! Opt-in native acceptance. Uses only its newly created session; always cleans up.
use jio_client::{SessionState, VmClient, VmSize};
use serde_json::json;
use std::{
    env,
    io::{self, Write},
    time::{Duration, Instant},
};

fn main() -> io::Result<()> {
    let client = VmClient::new(
        env::var("JIO_ENDPOINT").map_err(io::Error::other)?,
        env::var("JIO_API_KEY").map_err(io::Error::other)?,
    )?;
    let start = Instant::now();
    let vm = client.create_with_size(VmSize::Large)?;
    let create_ns = start.elapsed().as_nanos();
    let session = vm.session();
    println!(
        "{}",
        json!({"event":"created","session_id":session.session_id,"create_ns":create_ns,
        "template_id":session.template_id,"core_sha256":session.core_sha256,"vcpu":4,"memory_mib":8192,
        "runtime_timing_ns":{"cow_fork":session.cow_fork_ns,"guest_ready":session.guest_ready_ns,
        "storage_ready":session.storage_ready_ns,"network_ready":session.network_ready_ns,
        "session_ready":session.session_ready_ns,"ssh_ready":session.ssh_ready_ns}})
    );
    io::stdout().flush()?;
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let timeout = Duration::from_secs(30);
        let start = Instant::now();
        assert!(vm.exec("true", timeout)?.success());
        let attach_first_exec_ns = start.elapsed().as_nanos();
        let mut samples = Vec::new();
        for _ in 0..30 {
            let start = Instant::now();
            let result = vm.exec("printf jio", timeout)?;
            assert!(result.success());
            assert_eq!(result.stdout, b"jio");
            samples.push(start.elapsed().as_nanos());
        }
        let binary = b"hello\0jio\n";
        vm.write_file("/workspace/unified-access-test", binary, timeout)?;
        assert_eq!(
            vm.read_file("/workspace/unified-access-test", timeout)?,
            binary
        );
        let mut jobs = Vec::new();
        for _ in 0..4 {
            let vm = vm.clone();
            jobs.push(std::thread::spawn(move || {
                vm.exec("printf concurrent", timeout)
            }));
        }
        for job in jobs {
            assert_eq!(
                job.join()
                    .map_err(|_| io::Error::other("test thread failed"))??
                    .stdout,
                b"concurrent"
            );
        }
        let result = vm.exec("printf out; printf err >&2; exit 7", timeout)?;
        assert_eq!(result.exit_code, 7);
        assert_eq!(result.stdout, b"out");
        assert_eq!(result.stderr, b"err");
        assert_eq!(vm.stop()?.state, SessionState::Stopped);
        assert_eq!(vm.start()?.generation, 2);
        assert_eq!(
            vm.read_file("/workspace/unified-access-test", timeout)?,
            binary
        );
        println!(
            "{}",
            json!({"event":"passed","attach_first_exec_ns":attach_first_exec_ns,
            "warm_exec_ns":samples,"concurrency":1,"parallel_channels":4,"failures":0,
            "cache":"warm resident VM, authenticated SSH master reused","storage_stop_start":true})
        );
        Ok(())
    }))
    .unwrap_or_else(|_| Err(io::Error::other("native acceptance assertion failed")));
    let cleanup = vm.destroy();
    println!(
        "{}",
        json!({"event":"cleanup","session_id":vm.session().session_id,"confirmed":cleanup.is_ok()})
    );
    result.and(cleanup)
}
