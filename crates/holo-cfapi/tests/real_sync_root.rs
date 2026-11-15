//! Exercises the real Cloud Filter driver: registration, placeholder creation,
//! hydration triggered by another process, rename/delete notifications, pinning,
//! dehydration and identity detach.
//!
//! Registers a temporary sync root under `target/`, so it's `#[ignore]`d by default:
//! `cargo test -p holo-cfapi --test real_sync_root -- --ignored --nocapture`
#![cfg(windows)]

use std::{
    path::{Path, PathBuf},
    process::Command,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use holo_cfapi::{
    CallbackHandler, HydrateError, HydrationRequest, HydrationSink, LocalEvent, PinState,
    PlaceholderSpec, SyncRootSpec, placeholder, root,
};

/// Deterministic content: byte i = (i * 7 + 3) mod 251 (catches offset mistakes).
fn byte_at(i: u64) -> u8 {
    ((i * 7 + 3) % 251) as u8
}

#[derive(Default)]
struct TestHandler {
    requests: Mutex<Vec<HydrationRequest>>,
    events: Mutex<Vec<LocalEvent>>,
}

impl CallbackHandler for TestHandler {
    fn hydrate(
        &self,
        req: HydrationRequest,
        sink: &mut dyn HydrationSink,
    ) -> Result<(), HydrateError> {
        self.requests.lock().unwrap().push(req.clone());
        // Stream in odd-sized pieces to exercise the aligned writer.
        let mut pos = req.range.start;
        while pos < req.range.end {
            let n = 7777.min(req.range.end - pos);
            let chunk: Vec<u8> = (pos..pos + n).map(byte_at).collect();
            sink.push(&chunk)
                .map_err(|e| HydrateError::Other(e.to_string()))?;
            pos += n;
        }
        Ok(())
    }
    fn notify(&self, event: LocalEvent) {
        self.events.lock().unwrap().push(event);
    }
}

struct TestRoot {
    spec: SyncRootSpec,
}

impl Drop for TestRoot {
    fn drop(&mut self) {
        let _ = root::unregister(&self.spec);
        let _ = std::fs::remove_dir_all(&self.spec.path);
    }
}

fn powershell(script: &str) -> String {
    let out = Command::new("powershell.exe")
        .args(["-NoProfile", "-NonInteractive", "-Command", script])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "powershell failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_owned()
}

fn wait_for(what: &str, mut f: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !f() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn spec_file(id: &str, size: u64) -> PlaceholderSpec {
    PlaceholderSpec {
        is_dir: false,
        size,
        modified: chrono::Utc::now(),
        identity: id.as_bytes().to_vec(),
    }
}

#[test]
#[ignore = "registers a real Cloud Filter sync root"]
fn end_to_end_on_real_driver() {
    assert!(
        root::is_supported(),
        "Cloud Filter API not supported on this machine"
    );
    let dir: PathBuf = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/cf-it")
        .join(format!("root-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let dir = std::fs::canonicalize(&dir).unwrap();
    let dir = PathBuf::from(dir.to_string_lossy().trim_start_matches(r"\\?\"));
    let t = TestRoot {
        spec: SyncRootSpec {
            provider_name: "HoloFilesTest".into(),
            account_key: format!("it{}", std::process::id()),
            display_name: "holo integration test".into(),
            path: dir.clone(),
            icon: r"%SystemRoot%\system32\imageres.dll,-1043".into(),
            version: "0.0.1".into(),
            recycle_bin_url: None,
        },
    };
    root::register(&t.spec).expect("register");
    assert!(root::is_registered(&t.spec).unwrap());

    let handler = Arc::new(TestHandler::default());
    let _conn = root::connect(&dir, handler.clone(), 0).expect("connect");

    // --- placeholders ---------------------------------------------------------
    let size = 3 * 1024 * 1024 + 1234; // not page aligned
    placeholder::create(&dir, "big.bin", &spec_file("ID-BIG", size)).unwrap();
    placeholder::create(
        &dir,
        "folder",
        &PlaceholderSpec {
            is_dir: true,
            size: 0,
            modified: chrono::Utc::now(),
            identity: b"ID-DIR".to_vec(),
        },
    )
    .unwrap();
    placeholder::create(&dir.join("folder"), "small.txt", &spec_file("ID-SMALL", 10)).unwrap();

    let st = placeholder::state(&dir.join("big.bin")).unwrap().unwrap();
    let ph = st.placeholder.clone().unwrap();
    assert_eq!(st.size, size);
    assert!(
        ph.in_sync && ph.partial,
        "fresh placeholder is dehydrated: {ph:?}"
    );
    assert_eq!(ph.identity, b"ID-BIG");
    assert!(
        placeholder::state(&dir.join("folder"))
            .unwrap()
            .unwrap()
            .is_dir
    );

    // --- hydration from another process ---------------------------------------
    let file = dir.join("big.bin");
    let len = powershell(&format!(
        "[IO.File]::ReadAllBytes('{}').Length",
        file.display()
    ));
    assert_eq!(len.parse::<u64>().unwrap(), size);
    let data = std::fs::read(&file).unwrap();
    assert!(
        data.iter()
            .enumerate()
            .all(|(i, b)| *b == byte_at(i as u64)),
        "hydrated bytes match"
    );
    let reqs = handler.requests.lock().unwrap().clone();
    assert!(!reqs.is_empty());
    assert!(
        reqs.iter()
            .all(|r| r.range.start % 4096 == 0 && r.identity == b"ID-BIG")
    );
    let st = placeholder::state(&file).unwrap().unwrap();
    assert!(
        !st.placeholder.as_ref().unwrap().partial,
        "fully hydrated now"
    );
    assert!(st.placeholder.unwrap().in_sync, "reading keeps it in sync");

    // --- dehydrate ("free up space") ------------------------------------------
    placeholder::dehydrate(&file).unwrap();
    assert!(
        placeholder::state(&file)
            .unwrap()
            .unwrap()
            .placeholder
            .unwrap()
            .partial
    );

    // --- pin + explicit hydrate ("always keep on this device") ----------------
    placeholder::set_pin(&file, PinState::Pinned).unwrap();
    placeholder::hydrate(&file).unwrap();
    let ph = placeholder::state(&file)
        .unwrap()
        .unwrap()
        .placeholder
        .unwrap();
    assert_eq!(ph.pin, PinState::Pinned);
    assert!(!ph.partial);

    // --- remote content update on a pinned file (unpin → update → re-pin) -----
    let was_pinned = placeholder::update(&file, &spec_file("ID-BIG", 5000), true).unwrap();
    assert!(was_pinned);
    let st = placeholder::state(&file).unwrap().unwrap();
    assert_eq!(st.size, 5000);
    assert_eq!(st.placeholder.unwrap().pin, PinState::Pinned);

    // --- local edit clears in-sync; detach drops identity ---------------------
    let small = dir.join("folder").join("small.txt");
    powershell(&format!(
        "Set-Content -LiteralPath '{}' -Value 'edited' -NoNewline",
        small.display()
    ));
    let ph = placeholder::state(&small)
        .unwrap()
        .unwrap()
        .placeholder
        .unwrap();
    assert!(!ph.in_sync, "local write clears in-sync");
    placeholder::detach(&small).unwrap();
    assert!(
        placeholder::state(&small)
            .unwrap()
            .unwrap()
            .identity()
            .is_none()
    );

    // --- new regular file → mark_synced converts it ---------------------------
    let new = dir.join("new.txt");
    powershell(&format!(
        "Set-Content -LiteralPath '{}' -Value 'hello' -NoNewline",
        new.display()
    ));
    assert!(!placeholder::state(&new).unwrap().unwrap().is_placeholder());
    let usn = placeholder::usn(&new).unwrap();
    placeholder::mark_synced(&new, b"ID-NEW", Some(usn)).unwrap();
    let ph = placeholder::state(&new)
        .unwrap()
        .unwrap()
        .placeholder
        .unwrap();
    assert!(ph.in_sync);
    assert_eq!(ph.identity, b"ID-NEW");

    // --- rename / delete from another process notify the engine ---------------
    powershell(&format!(
        "Rename-Item -LiteralPath '{}' -NewName 'renamed.txt'",
        new.display()
    ));
    powershell(&format!(
        "Remove-Item -LiteralPath '{}'",
        dir.join("renamed.txt").display()
    ));
    wait_for("rename + delete notifications", || {
        let ev = handler.events.lock().unwrap();
        ev.iter()
            .any(|e| matches!(e, LocalEvent::Renamed { to, .. } if to.ends_with("renamed.txt")))
            && ev
                .iter()
                .any(|e| matches!(e, LocalEvent::Deleted { path } if path.ends_with("renamed.txt")))
    });
    wait_for("change notification for the edited file", || {
        handler.events.lock().unwrap().iter().any(
            |e| matches!(e, LocalEvent::Changed(p) if p.iter().any(|p| p.ends_with("small.txt"))),
        )
    });
}
