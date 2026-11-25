// printpapi — self-hosted PrintNode alternative. Elastic License 2.0 (see LICENSE).
//! Scenarios of `update`: refusals that change nothing, the happy path, every way back, and the
//! unattended run that leaves quietly.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use ring::signature::Ed25519KeyPair;

use super::machine::*;
use super::*;

fn staged(sim: &Sim) -> Outcome {
    update(Source::Staged(&sim.staging), &sim.env())
}

fn fetched(sim: &Sim) -> Outcome {
    update(Source::Fetch, &sim.env())
}

/// A sim with the good release staged.
fn ready_to_install(os: Os) -> Sim {
    let sim = Sim::new(os);
    sim.stage(&sim.good_release());
    sim
}

fn has(log: &[String], needle: &str) -> bool {
    log.iter().any(|l| l.contains(needle))
}

fn nothing_stopped_or_started(sim: &Sim) {
    let log = sim.log();
    for word in [
        "systemctl restart",
        "systemctl stop",
        "/end",
        "/run",
        "kickstart",
        "enable",
        "/create",
        "bootout",
        "bootstrap",
    ] {
        assert!(!has(&log, word), "{word}: {log:?}");
    }
}

fn state(sim: &Sim) -> State {
    read_state(&sim.layout).expect("the state file")
}

// ---- the way it goes ----

#[test]
fn an_update_swaps_both_programs_writes_the_marker_and_leaves_only_the_lock() {
    for os in [Os::Linux, Os::Windows] {
        let sim = ready_to_install(os);
        let ini = std::fs::read(&sim.layout.ini).unwrap();
        let unit = std::fs::read(&sim.layout.autostart).unwrap();
        let out = staged(&sim).unwrap();
        assert_eq!(out, Some(Version(3, 7, 0)), "{os:?}");
        assert_eq!(sim.binary(), NEW_AGENT);
        assert_eq!(sim.tray(), NEW_TRAY);
        assert_eq!(sim.marker_text(), "version=3.7.0\n");
        // nothing else on the machine was written
        assert_eq!(std::fs::read(&sim.layout.ini).unwrap(), ini);
        assert_eq!(std::fs::read(&sim.layout.autostart).unwrap(), unit);
        // the private folder holds only the lock, the install folder no leftover
        let mut names: Vec<_> = std::fs::read_dir(private_dir(&sim.layout))
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        assert_eq!(names, [LOCK_NAME]);
        for p in [&sim.layout.binary, &sim.layout.tray] {
            for left in leftovers_of(p) {
                assert!(!left.exists(), "{os:?}: {}", left.display());
            }
        }
        let st = state(&sim);
        assert_eq!(
            (st.version.as_str(), st.from.as_str(), st.ok, st.tries),
            ("3.7.0", "3.6.0", true, 0)
        );
        // every leftover could be deleted at once: nothing waits for a reboot
        assert!(sim.at_reboot.borrow().is_empty(), "{os:?}");
        let log = sim.log();
        // both programs were started once before anything stopped
        let probes: Vec<_> = log.iter().filter(|l| l.starts_with("probe ")).collect();
        assert_eq!(probes.len(), 2, "{log:?}");
        // never what an update must not do
        for word in ["enable", "/create", "bootout", "bootstrap"] {
            assert!(!has(&log, word), "{os:?} {word}: {log:?}");
        }
    }
}

#[test]
fn linux_renames_first_and_restarts_once_while_windows_stops_waits_renames_and_runs() {
    let sim = ready_to_install(Os::Linux);
    staged(&sim).unwrap();
    let log: Vec<_> = sim
        .log()
        .into_iter()
        .filter(|l| !l.starts_with("probe"))
        .collect();
    assert_eq!(log, ["systemctl restart printpapi-agent"]);

    let sim = ready_to_install(Os::Windows);
    staged(&sim).unwrap();
    let log: Vec<_> = sim
        .log()
        .into_iter()
        .filter(|l| !l.starts_with("probe"))
        .collect();
    assert!(
        log[0].contains("schtasks.exe /end /tn printpapi-agent"),
        "{log:?}"
    );
    assert!(log[1].contains("/query"), "{log:?}");
    assert!(
        log.last()
            .unwrap()
            .contains("schtasks.exe /run /tn printpapi-agent"),
        "{log:?}"
    );
}

#[test]
fn the_old_programs_are_kept_until_the_new_ones_proved_themselves() {
    let sim = ready_to_install(Os::Linux);
    let seen = Rc::new(RefCell::new(Vec::new()));
    let (binary, l) = (sim.layout.binary.clone(), sim.layout.clone());
    let s = seen.clone();
    *sim.on_sleep.borrow_mut() = Some(Box::new(move |_| {
        // during the health wait the old agent file exists as .old
        s.borrow_mut().push(
            leftovers_of(&binary)[2].exists() && std::fs::read(&l.binary).unwrap() == NEW_AGENT,
        );
    }));
    staged(&sim).unwrap();
    assert!(seen.borrow().iter().any(|x| *x));
}

#[test]
fn a_staged_file_changed_after_it_was_read_does_not_matter() {
    let sim = ready_to_install(Os::Linux);
    let name = format!("printpapi-update-agent-{}", target(Os::Linux));
    let path = sim.staging.join(name);
    *sim.on_probe.borrow_mut() = Some(Box::new(move |_| {
        let _ = std::fs::write(&path, b"swapped by a user after the check");
    }));
    staged(&sim).unwrap();
    assert_eq!(sim.binary(), NEW_AGENT);
}

#[test]
fn the_agent_of_a_release_without_a_tray_is_replaced_alone() {
    let sim = Sim::new(Os::Linux);
    std::fs::remove_file(&sim.layout.tray).unwrap();
    sim.stage(&sim.release("3.7.0", NEW_AGENT, Some(NEW_TRAY)));
    staged(&sim).unwrap();
    assert_eq!(sim.binary(), NEW_AGENT);
    assert!(
        !sim.layout.tray.exists(),
        "no tray installed, none put in place"
    );
}

// ---- refusals change nothing ----

fn refused(sim: &Sim, expected: &str) {
    let before = sim.snapshot();
    let e = error(staged(sim));
    assert_eq!(e.code(), expected, "{e}");
    assert_eq!(sim.snapshot(), before, "{expected}: the machine changed");
    nothing_stopped_or_started(sim);
}

#[test]
fn an_installation_the_setup_did_not_make_is_left_alone() {
    let sim = ready_to_install(Os::Linux);
    std::fs::remove_file(&sim.layout.marker).unwrap();
    refused(&sim, "system");
    // a marker that is a folder is no marker
    std::fs::create_dir(&sim.layout.marker).unwrap();
    refused(&sim, "system");
}

#[test]
fn a_build_without_a_key_installs_nothing() {
    let mut sim = ready_to_install(Os::Linux);
    sim.keys.clear();
    refused(&sim, "no_key");
}

#[test]
fn a_program_that_is_not_the_installed_one_installs_nothing() {
    let mut sim = ready_to_install(Os::Linux);
    let other = sim.tmp.path().join("copy-of-agent");
    std::fs::write(&other, OLD_AGENT).unwrap();
    sim.self_exe = other;
    refused(&sim, "system");
}

#[test]
fn an_older_or_equal_version_is_refused_and_a_newer_minor_is_not() {
    for (version, ok) in [
        ("3.5.9", false),
        ("3.6.0", false),
        ("2.99.99", false),
        ("3.6.1", true),
        ("3.10.0", true),
    ] {
        let sim = Sim::new(Os::Linux);
        sim.stage(&sim.release(version, NEW_AGENT, Some(NEW_TRAY)));
        if ok {
            // the probe wants the version of the file, which the test agent states as 3.7.0
            let release = sim.release(
                version,
                format!("agent {version}").as_bytes(),
                Some(format!("tray {version}").as_bytes()),
            );
            sim.stage(&release);
            assert_eq!(staged(&sim).unwrap().unwrap().to_string(), version);
        } else {
            refused(&sim, "not_newer");
        }
    }
}

#[test]
fn a_flipped_manifest_is_a_bad_signature_and_installs_nothing() {
    let sim = ready_to_install(Os::Linux);
    let manifest = sim.staging.join(MANIFEST_NAME);
    let good = std::fs::read(&manifest).unwrap();
    let mut flipped = good.clone();
    *flipped.last_mut().unwrap() ^= 1;
    std::fs::write(&manifest, flipped).unwrap();
    refused(&sim, "bad_signature");
}

#[test]
fn a_second_schema_under_a_good_signature_needs_the_new_setup_and_installs_nothing() {
    let sim = ready_to_install(Os::Linux);
    let manifest = sim.staging.join(MANIFEST_NAME);
    let text = String::from_utf8(std::fs::read(&manifest).unwrap()).unwrap();
    let payload = text
        .split_once("\n\n")
        .unwrap()
        .1
        .replace("\"schema\":1", "\"schema\":2");
    assert!(payload.contains("\"schema\":2"));
    let pair = Ed25519KeyPair::from_seed_unchecked(SEED).unwrap();
    let signed = format!(
        "sig {}\n\n{payload}",
        hex(pair.sign(payload.as_bytes()).as_ref())
    );
    std::fs::write(&manifest, signed).unwrap();
    refused(&sim, "manual");
}

#[test]
fn another_key_is_a_bad_signature() {
    let mut sim = ready_to_install(Os::Linux);
    sim.keys = vec![[9; 32]];
    refused(&sim, "bad_signature");
}

#[test]
fn a_missing_manifest_a_missing_file_and_a_folder_in_place_of_a_file_install_nothing() {
    let sim = ready_to_install(Os::Linux);
    let agent = sim
        .staging
        .join(format!("printpapi-update-agent-{}", target(Os::Linux)));
    std::fs::remove_file(&agent).unwrap();
    refused(&sim, "bad_file");
    std::fs::create_dir(&agent).unwrap();
    refused(&sim, "bad_file");
    std::fs::remove_file(sim.staging.join(MANIFEST_NAME)).unwrap();
    refused(&sim, "bad_file");
}

#[test]
fn a_wrong_hash_a_longer_file_and_a_shorter_file_install_nothing() {
    for bytes in [
        &b"agent 3.7.1"[..],
        b"agent 3.7.0 and more bytes",
        b"agent 3.7",
    ] {
        let sim = ready_to_install(Os::Linux);
        let agent = sim
            .staging
            .join(format!("printpapi-update-agent-{}", target(Os::Linux)));
        std::fs::write(&agent, bytes).unwrap();
        refused(&sim, "bad_file");
    }
    // the tray, too
    let sim = ready_to_install(Os::Linux);
    std::fs::write(
        sim.staging
            .join(format!("printpapi-update-tray-{}", target(Os::Linux))),
        b"tray 3.7.1",
    )
    .unwrap();
    refused(&sim, "bad_file");
}

#[test]
fn a_linked_staged_file_is_not_followed() {
    let sim = ready_to_install(Os::Linux);
    let agent = sim
        .staging
        .join(format!("printpapi-update-agent-{}", target(Os::Linux)));
    let real = sim.tmp.path().join("real-agent");
    std::fs::rename(&agent, &real).unwrap();
    #[cfg(unix)]
    let linked = std::os::unix::fs::symlink(&real, &agent).is_ok();
    #[cfg(windows)]
    let linked = std::os::windows::fs::symlink_file(&real, &agent).is_ok();
    if !linked {
        // creating a link needs a privilege on Windows; without it there is nothing to refuse
        return;
    }
    // the staging folder check refuses a link before any file is read
    refused(&sim, "bad_staging");
}

#[test]
fn read_regular_refuses_a_link_on_its_own() {
    let tmp = tempfile::tempdir().unwrap();
    let real = tmp.path().join("real");
    let link = tmp.path().join("link");
    std::fs::write(&real, b"bytes").unwrap();
    assert_eq!(read_regular(&real, 100).unwrap(), b"bytes");
    #[cfg(unix)]
    let linked = std::os::unix::fs::symlink(&real, &link).is_ok();
    #[cfg(windows)]
    let linked = std::os::windows::fs::symlink_file(&real, &link).is_ok();
    if !linked {
        return;
    }
    assert_eq!(read_regular(&link, 100).unwrap_err().code(), "bad_file");
}

#[test]
fn a_manifest_without_a_file_for_this_system_installs_nothing() {
    let sim = Sim::new(Os::Linux);
    let mut release = sim.release("3.7.0", NEW_AGENT, Some(NEW_TRAY));
    // the same release, signed, but for another architecture
    let t = target(Os::Linux);
    let text = String::from_utf8(release[0].1.clone()).unwrap();
    let signed_for_other = {
        let payload = text
            .split("\n\n")
            .nth(1)
            .unwrap()
            .replace(&t, "linux-sparc");
        let pair =
            Ed25519KeyPair::from_seed_unchecked(b"printpapi test seed, not secret!").unwrap();
        format!(
            "sig {}\n\n{payload}",
            hex(pair.sign(payload.as_bytes()).as_ref())
        )
        .into_bytes()
    };
    release[0].1 = signed_for_other;
    sim.stage(&release);
    refused(&sim, "no_target");
}

#[test]
fn a_branded_installation_is_refused_before_anything_is_fetched_or_stopped() {
    for marker in [
        "version=3.6.0\nbrand=acme\n",
        "version=3.6.0\nbrand=Not Valid\n",
        "garbage\n",
    ] {
        let sim = ready_to_install(Os::Linux);
        std::fs::write(&sim.layout.marker, marker).unwrap();
        refused(&sim, "manual");
        // the unattended run leaves quietly and does not even ask the network
        sim.serve(DEFAULT_BASE, &sim.good_release());
        assert_eq!(fetched(&sim).unwrap(), None);
        assert!(sim.downloaded.borrow().is_empty());
        nothing_stopped_or_started(&sim);
    }
}

#[test]
fn a_held_lock_is_busy_and_an_unattended_run_just_leaves() {
    let sim = ready_to_install(Os::Linux);
    let held = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(lock_path(&sim.layout))
        .unwrap();
    held.try_lock().unwrap();
    refused(&sim, "busy");
    // five tries, four waits of a second
    assert_eq!(sim.clock.get(), 10_004.0);
    sim.serve(DEFAULT_BASE, &sim.good_release());
    assert_eq!(fetched(&sim).unwrap(), None);
    drop(held);
    staged(&sim).unwrap();
}

#[test]
fn an_installation_without_a_lock_asks_for_the_setup() {
    let sim = ready_to_install(Os::Linux);
    std::fs::remove_dir_all(private_dir(&sim.layout)).unwrap();
    let before = sim.snapshot();
    let e = error(staged(&sim));
    assert_eq!(e.code(), "system");
    assert!(e.to_string().contains("Run the setup"), "{e}");
    assert_eq!(sim.snapshot(), before);
}

#[test]
fn a_binary_replaced_while_waiting_for_the_lock_is_busy_for_a_click() {
    let sim = ready_to_install(Os::Linux);
    let holder = Rc::new(RefCell::new(Some(
        std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(lock_path(&sim.layout))
            .unwrap(),
    )));
    holder.borrow().as_ref().unwrap().try_lock().unwrap();
    let (h, binary) = (holder.clone(), sim.layout.binary.clone());
    *sim.on_sleep.borrow_mut() = Some(Box::new(move |_| {
        // the other command ends and a setup replaced the program meanwhile
        std::fs::write(&binary, b"agent 3.6.5").unwrap();
        h.borrow_mut().take();
    }));
    let e = error(staged(&sim));
    assert_eq!(e.code(), "busy");
    nothing_stopped_or_started(&sim);
    assert_eq!(std::fs::read(&sim.layout.binary).unwrap(), b"agent 3.6.5");
}

// ---- the programs are started before anything changes ----

#[test]
fn a_program_that_will_not_start_stops_the_update_before_anything_changes() {
    for (needle, error_text) in [
        ("agent 3.7.0", "blocked by policy"),
        ("tray 3.7.0", "timed out after 60 seconds"),
    ] {
        let sim = ready_to_install(Os::Linux);
        sim.probe_error
            .borrow_mut()
            .push((needle.into(), error_text.into()));
        let before = sim.snapshot();
        let e = error(staged(&sim));
        assert_eq!(e.code(), "wont_start", "{needle}");
        assert!(e.to_string().contains("Nothing was changed"), "{e}");
        assert_eq!(sim.snapshot(), before, "{needle}: no .new file stays");
        nothing_stopped_or_started(&sim);
        assert!(
            !state_path(&sim.layout).exists(),
            "a failure before the stop is not recorded"
        );
    }
}

#[test]
fn a_program_that_reports_another_version_will_not_be_installed() {
    let sim = Sim::new(Os::Linux);
    // the manifest says 3.7.0, the program says 3.9.9
    sim.stage(&sim.release("3.7.0", b"agent 3.9.9", Some(NEW_TRAY)));
    let before = sim.snapshot();
    assert_eq!(code(staged(&sim)), "wont_start");
    assert_eq!(sim.snapshot(), before);
    let sim = Sim::new(Os::Linux);
    sim.stage(&sim.release("3.7.0", NEW_AGENT, Some(b"tray 3.9.9")));
    assert_eq!(code(staged(&sim)), "wont_start");
    nothing_stopped_or_started(&sim);
}

#[test]
fn a_program_that_is_no_program_will_not_be_installed() {
    let sim = Sim::new(Os::Linux);
    sim.stage(&sim.release("3.7.0", b"junk", Some(NEW_TRAY)));
    assert_eq!(code(staged(&sim)), "wont_start");
}

// ---- waiting for a quiet agent ----

#[test]
fn an_agent_that_is_not_connected_is_never_updated() {
    let sim = ready_to_install(Os::Linux);
    sim.no_status.set(true);
    refused(&sim, "offline");
    let sim = ready_to_install(Os::Linux);
    // a status that is older than two minutes
    sim.clock.set(sim.clock.get());
    sim.proc_.borrow_mut().version = None;
    sim.proc_.borrow_mut().stopped_at = 1.0;
    refused(&sim, "offline");
}

#[test]
fn a_busy_agent_is_waited_for_and_a_job_that_never_ends_gives_printing() {
    let sim = ready_to_install(Os::Linux);
    sim.busy_until.set(sim.clock.get() + 30.0);
    staged(&sim).unwrap();
    assert!(sim.clock.get() >= 10_030.0, "waited for the job");

    let sim = ready_to_install(Os::Linux);
    sim.busy_until.set(f64::INFINITY);
    let before = sim.snapshot();
    assert_eq!(code(staged(&sim)), "printing");
    assert_eq!(sim.snapshot(), before);
    assert!(
        sim.clock.get() >= 10_120.0 && sim.clock.get() < 10_125.0,
        "two minutes: {}",
        sim.clock.get()
    );
    nothing_stopped_or_started(&sim);
}

#[test]
fn an_agent_that_prints_for_over_two_minutes_is_waited_for_not_called_offline() {
    // a long print does not refresh last_contact: the file is fresh and the agent says busy, but
    // the last contact with the server is older than two minutes
    let sim = ready_to_install(Os::Linux);
    let clock = sim.clock.clone();
    let start = clock.get();
    let read = move |_: &Path| {
        let now = clock.get();
        let busy = now < start + 30.0;
        let contact = if busy { now - 200.0 } else { now - 0.5 };
        Some(StatusSeen {
            age: 1.0,
            text: format!(
                r#"{{"version":"3.6.0","state":"online","started_at":9000.0,"last_contact":{contact},"busy":{busy}}}"#
            ),
        })
    };
    let mut env = sim.env();
    env.read_status = &read;
    wait_quiet(&env).unwrap();
    assert!(sim.clock.get() >= start + 30.0, "waited for the print");

    // still busy after two minutes: printing, not offline
    let clock = sim.clock.clone();
    let read = move |_: &Path| {
        Some(StatusSeen {
            age: 1.0,
            text: format!(
                r#"{{"version":"3.6.0","state":"online","started_at":9000.0,"last_contact":{},"busy":true}}"#,
                clock.get() - 200.0
            ),
        })
    };
    let mut env = sim.env();
    env.read_status = &read;
    assert_eq!(wait_quiet(&env).unwrap_err().code(), "printing");
}

#[test]
fn two_quiet_looks_are_needed_and_a_job_between_them_starts_the_count_again() {
    let sim = ready_to_install(Os::Linux);
    // quiet at the first look, busy at the second, quiet after that
    let calls = Rc::new(Cell::new(0));
    let busy = sim.busy_until.clone();
    let clock = sim.clock.clone();
    let c = calls.clone();
    *sim.on_sleep.borrow_mut() = Some(Box::new(move |_| {
        c.set(c.get() + 1);
        if c.get() == 1 {
            busy.set(clock.get() + 3.0);
        }
    }));
    staged(&sim).unwrap();
    assert!(sim.clock.get() >= 10_004.0);
}

// ---- Windows: stop, wait, swap, run ----

#[test]
fn a_task_that_is_still_running_after_the_wait_is_started_again_and_nothing_is_swapped() {
    let sim = ready_to_install(Os::Windows);
    sim.stuck_running.set(true);
    let e = error(staged(&sim));
    assert_eq!(e.code(), "system");
    assert!(e.to_string().contains("did not stop"), "{e}");
    assert_eq!(sim.binary(), OLD_AGENT);
    assert_eq!(sim.tray(), OLD_TRAY);
    let log: Vec<_> = sim
        .log()
        .into_iter()
        .filter(|l| !l.starts_with("probe"))
        .collect();
    assert!(log.last().unwrap().contains("/run"), "{log:?}");
    assert_eq!(log.iter().filter(|l| l.contains("/query")).count(), 15);
    assert!(
        !sim.layout
            .binary
            .with_file_name(".printpapi-agent.exe.new")
            .exists()
    );
}

#[test]
fn a_failing_query_counts_as_unknown_and_is_waited_for_the_full_fifteen_seconds() {
    let sim = ready_to_install(Os::Windows);
    sim.query_fails.set(true);
    let start = sim.clock.get();
    staged(&sim).unwrap();
    let log = sim.log();
    assert_eq!(log.iter().filter(|l| l.contains("/query")).count(), 15);
    // 2 quiet looks (1 s) + 15 s of waiting + the health wait
    assert!(
        sim.clock.get() - start >= 16.0 + 15.0,
        "{}",
        sim.clock.get() - start
    );
}

#[test]
fn a_start_the_scheduler_ignored_is_given_once_more_after_twenty_seconds() {
    let sim = ready_to_install(Os::Windows);
    sim.ignore_first_start.set(true);
    staged(&sim).unwrap();
    let runs = sim.log().into_iter().filter(|l| l.contains("/run")).count();
    assert_eq!(runs, 2);
    assert!(sim.clock.get() >= 10_020.0);
}

#[test]
fn only_the_stop_and_the_start_run_on_macos_and_the_app_is_swapped_by_rename() {
    let sim = ready_to_install(Os::Macos);
    staged(&sim).unwrap();
    let log: Vec<_> = sim
        .log()
        .into_iter()
        .filter(|l| !l.starts_with("probe"))
        .collect();
    assert!(
        has(&log, "kickstart -k system/com.printpapi.agent"),
        "{log:?}"
    );
    for word in ["bootout", "bootstrap", "enable", "/create"] {
        assert!(!has(&log, word), "{word}");
    }
    assert!(has(&log, "ditto -x -k"), "{log:?}");
    let app = sim
        .layout
        .tray
        .join("Contents")
        .join("MacOS")
        .join("printpapi-setup");
    assert_eq!(std::fs::read(app).unwrap(), NEW_TRAY);
}

#[test]
fn an_app_archive_with_two_apps_or_none_is_refused_before_anything_changes() {
    for count in [0, 2] {
        let sim = ready_to_install(Os::Macos);
        sim.unpack_apps.set(count);
        let before = sim.snapshot();
        assert_eq!(code(staged(&sim)), "wont_start", "{count}");
        assert_eq!(sim.snapshot(), before, "{count}");
        nothing_stopped_or_started(&sim);
    }
}

// ---- the way back ----

fn rolled_back(sim: &Sim, head: &str, tail: &str) -> ApplyError {
    let e = error(staged(sim));
    assert_eq!(e.code(), "rolled_back", "{e}");
    let text = e.to_string();
    assert!(text.contains(head), "{text}");
    assert!(text.ends_with(tail), "{text}");
    assert_eq!(sim.binary(), OLD_AGENT, "the old agent is back");
    assert_eq!(sim.tray(), OLD_TRAY, "the old tray is back");
    assert_eq!(
        sim.marker_text(),
        "version=3.6.0\n",
        "the marker did not move"
    );
    for p in [&sim.layout.binary, &sim.layout.tray] {
        for left in leftovers_of(p) {
            assert!(!left.exists(), "{}", left.display());
        }
    }
    let st = state(sim);
    assert!(!st.ok);
    assert_eq!(st.version, "3.7.0");
    e
}

#[test]
fn a_new_agent_that_never_connects_is_replaced_by_the_old_one() {
    for os in [Os::Linux, Os::Windows] {
        let sim = ready_to_install(os);
        sim.new_agent.set(NewAgent::NeverOnline);
        let e = rolled_back(
            &sim,
            "Version 3.7.0 did not connect, so 3.6.0 was put back.",
            "It is running again.",
        );
        // the sentence of the docs, word for word; the detail is for the log
        assert_eq!(
            e.to_string(),
            "Version 3.7.0 did not connect, so 3.6.0 was put back. It is running again."
        );
        let logged = crate::ERR_LINES.with(|l| l.borrow().join("\n"));
        assert!(
            logged.contains(&format!("{HEALTH_SECS} seconds")),
            "{logged}"
        );
        assert_eq!(state(&sim).tries, 1);
        // the new agent, the old agent, and on Windows the new agent again after twenty seconds
        // (a start the task ignores is given once more; `systemctl restart` would end a slow agent)
        let expected = if os == Os::Windows { 3 } else { 2 };
        let starts = sim
            .log()
            .into_iter()
            .filter(|l| l.contains("/run") || l.contains("systemctl restart"))
            .count();
        assert_eq!(starts, expected, "{os:?}");
        // the health wait ended at the limit, and not much later
        assert!(sim.clock.get() - 10_000.0 < HEALTH_SECS as f64 + 400.0);
    }
}

/// The raw state file: what an update leaves for a person who looks for the reason.
fn state_json(sim: &Sim) -> serde_json::Value {
    serde_json::from_slice(&std::fs::read(state_path(&sim.layout)).unwrap()).unwrap()
}

#[test]
fn the_reason_behind_a_rolled_back_update_is_kept_in_the_state_file() {
    // the task runs as SYSTEM without a console and a click runs hidden: stderr is not a log
    let sim = ready_to_install(Os::Windows);
    sim.new_agent.set(NewAgent::NeverOnline);
    let e = error(staged(&sim));
    let st = state_json(&sim);
    // the sentence of the docs stays the message, the detail is a field of its own
    assert_eq!(st["message"], e.to_string());
    let detail = st["detail"].as_str().unwrap();
    assert!(
        detail.contains(&format!("{HEALTH_SECS} seconds")),
        "{detail}"
    );
    assert!(!e.to_string().contains("seconds"), "{e}");
    // read_state does not mind the extra field
    assert!(read_state(&sim.layout).is_some());

    // an old agent that cannot be started names that too
    let sim = ready_to_install(Os::Linux);
    sim.new_agent.set(NewAgent::NeverOnline);
    let (fails, armed, clock) = (
        sim.start_failures.clone(),
        Cell::new(false),
        sim.clock.clone(),
    );
    *sim.on_sleep.borrow_mut() = Some(Box::new(move |_| {
        if !armed.get() && clock.get() >= 10_000.0 + HEALTH_SECS as f64 {
            armed.set(true);
            fails.set(1000);
        }
    }));
    error(staged(&sim));
    let detail = state_json(&sim)["detail"].as_str().unwrap().to_string();
    assert!(
        detail.contains("could not be started: start failed"),
        "{detail}"
    );

    // a success carries no detail
    let sim = ready_to_install(Os::Linux);
    staged(&sim).unwrap();
    assert_eq!(state_json(&sim)["detail"], "");
}

#[test]
fn a_new_agent_that_reports_the_old_version_is_not_taken_for_new() {
    let sim = ready_to_install(Os::Linux);
    sim.new_agent.set(NewAgent::WrongVersion);
    rolled_back(&sim, "did not connect", "It is running again.");
}

#[test]
fn a_crash_loop_is_caught_by_the_second_look() {
    let sim = ready_to_install(Os::Linux);
    sim.new_agent.set(NewAgent::CrashLoop);
    rolled_back(
        &sim,
        "did not connect, so 3.6.0 was put back.",
        "It is running again.",
    );
    let logged = crate::ERR_LINES.with(|l| l.borrow().join("\n"));
    assert!(logged.contains("did not stay connected"), "{logged}");
}

#[test]
fn a_start_that_fails_is_a_way_back_too() {
    let sim = ready_to_install(Os::Linux);
    sim.start_failures.set(1);
    rolled_back(&sim, "the new agent did not start", "It is running again.");
}

#[test]
fn a_tray_that_cannot_be_put_in_place_takes_the_agent_back_too() {
    let sim = ready_to_install(Os::Linux);
    // the start command of the agent makes the staged tray file disappear: the rename fails
    *sim.remove_on_start.borrow_mut() = Some(leftover_of(&sim.layout.tray));
    rolled_back(
        &sim,
        "cannot put the new printpapi-tray in place",
        "It is running again.",
    );
}

#[test]
fn a_new_agent_that_is_busy_is_given_time_to_finish_before_it_is_replaced() {
    let sim = ready_to_install(Os::Linux);
    *sim.remove_on_start.borrow_mut() = Some(leftover_of(&sim.layout.tray));
    let (busy, clock, armed) = (sim.busy_until.clone(), sim.clock.clone(), Cell::new(false));
    *sim.on_sleep.borrow_mut() = Some(Box::new(move |_| {
        // the new agent is online and starts a job in the middle of the second look
        if !armed.get() && clock.get() >= 10_020.0 {
            armed.set(true);
            busy.set(clock.get() + 40.0);
        }
    }));
    rolled_back(
        &sim,
        "cannot put the new printpapi-tray in place",
        "It is running again.",
    );
    assert!(sim.clock.get() >= 10_000.0 + 40.0);
}

#[test]
fn a_failing_start_of_the_old_agent_is_tried_again_and_again() {
    let sim = ready_to_install(Os::Linux);
    sim.new_agent.set(NewAgent::NeverOnline);
    let (fails, armed, clock) = (
        sim.start_failures.clone(),
        Cell::new(false),
        sim.clock.clone(),
    );
    *sim.on_sleep.borrow_mut() = Some(Box::new(move |_| {
        // the health wait is over: from here on the old agent is started
        if !armed.get() && clock.get() >= 10_000.0 + HEALTH_SECS as f64 {
            armed.set(true);
            fails.set(2);
        }
    }));
    rolled_back(&sim, "did not connect", "It is running again.");
    let starts = sim
        .log()
        .into_iter()
        .filter(|l| l.contains("systemctl restart"))
        .count();
    // the new one, two failures of the old one, then it works
    assert_eq!(starts, 1 + 3);
}

#[test]
fn a_start_of_the_old_agent_that_the_windows_task_ignored_is_given_once_more() {
    let sim = ready_to_install(Os::Windows);
    sim.new_agent.set(NewAgent::NeverOnline);
    sim.ignore_first_old.set(true);
    rolled_back(&sim, "did not connect", "It is running again.");
    let starts = sim.log().into_iter().filter(|l| l.contains("/run")).count();
    // the new agent, the new agent again after twenty seconds, the old agent (ignored), the old agent again
    assert_eq!(starts, 4);
}

#[test]
fn a_slow_new_agent_is_not_restarted_where_a_restart_would_end_it() {
    // Linux and macOS: the start command is a restart, so it is given once and not again
    for os in [Os::Linux, Os::Macos] {
        let sim = ready_to_install(os);
        sim.new_agent.set(NewAgent::NeverOnline);
        rolled_back(&sim, "did not connect", "It is running again.");
        let restarts = sim
            .log()
            .into_iter()
            .filter(|l| l.contains("systemctl restart") || l.contains("kickstart"))
            .count();
        assert_eq!(
            restarts, 2,
            "{os:?}: the new agent once, the old agent once"
        );
    }
}

#[test]
fn an_old_agent_that_cannot_be_started_at_all_is_said_to_be_not_running() {
    let sim = ready_to_install(Os::Linux);
    sim.new_agent.set(NewAgent::NeverOnline);
    let (fails, armed, clock) = (
        sim.start_failures.clone(),
        Cell::new(false),
        sim.clock.clone(),
    );
    *sim.on_sleep.borrow_mut() = Some(Box::new(move |_| {
        if !armed.get() && clock.get() >= 10_000.0 + HEALTH_SECS as f64 {
            armed.set(true);
            fails.set(1000);
        }
    }));
    rolled_back(
        &sim,
        "did not connect",
        "The agent is not running. Run the setup again.",
    );
    let tries = sim
        .log()
        .into_iter()
        .filter(|l| l.contains("systemctl restart"))
        .count();
    assert_eq!(tries, 1 + ROLLBACK_START_TRIES as usize);
}

#[test]
fn an_old_agent_that_starts_but_does_not_connect_says_so() {
    let sim = ready_to_install(Os::Linux);
    sim.new_agent.set(NewAgent::NeverOnline);
    // the status file disappears once the swap is back: the old agent never reports
    let (no_status, clock, armed) = (sim.no_status.clone(), sim.clock.clone(), Cell::new(false));
    *sim.on_sleep.borrow_mut() = Some(Box::new(move |_| {
        if !armed.get() && clock.get() >= 10_000.0 + HEALTH_SECS as f64 {
            armed.set(true);
            no_status.set(true);
        }
    }));
    rolled_back(
        &sim,
        "did not connect",
        "It is running but has not connected yet; see the log.",
    );
}

#[test]
fn a_rename_that_fails_is_named_and_ends_in_the_not_running_text() {
    let sim = ready_to_install(Os::Linux);
    sim.new_agent.set(NewAgent::NeverOnline);
    let (binary, clock, armed) = (
        sim.layout.binary.clone(),
        sim.clock.clone(),
        Cell::new(false),
    );
    *sim.on_sleep.borrow_mut() = Some(Box::new(move |_| {
        // somebody removed the aside copy of the old agent: it cannot be put back
        if !armed.get() && clock.get() >= 10_000.0 + HEALTH_SECS as f64 {
            armed.set(true);
            let _ = std::fs::remove_file(binary.with_file_name(".printpapi-agent.old"));
        }
    }));
    let e = error(staged(&sim));
    let text = e.to_string();
    assert_eq!(e.code(), "rolled_back");
    // some program is in place, not none
    assert!(sim.layout.binary.exists());
    assert!(text.contains("putting 3.6.0 back failed"), "{text}");
    assert!(
        text.contains("printpapi-agent could not be put back"),
        "{text}"
    );
    assert!(
        text.ends_with("The agent is not running. Run the setup again."),
        "{text}"
    );
}

#[test]
fn a_put_back_that_fails_inside_the_swap_is_the_not_running_case() {
    let sim = ready_to_install(Os::Linux);
    // the first sleep (the quiet look) takes the prepared new agent away, so the second rename of
    // the swap cannot work; the next sleeps (its retries) take away the aside copy of the old agent
    let (new, old) = (
        sim.layout.binary.with_file_name(".printpapi-agent.new"),
        sim.layout.binary.with_file_name(".printpapi-agent.old"),
    );
    let calls = Rc::new(Cell::new(0));
    let c = calls.clone();
    *sim.on_sleep.borrow_mut() = Some(Box::new(move |_| {
        c.set(c.get() + 1);
        if c.get() == 1 {
            let _ = std::fs::remove_file(&new);
        } else {
            let _ = std::fs::remove_file(&old);
        }
    }));
    let e = error(staged(&sim));
    let text = e.to_string();
    assert_eq!(e.code(), "rolled_back", "{text}");
    assert!(text.contains("putting 3.6.0 back failed"), "{text}");
    assert!(text.contains(".printpapi-agent.old"), "{text}");
    assert!(
        text.ends_with("The agent is not running. Run the setup again."),
        "{text}"
    );
    // nothing was started: there is no program to start
    assert!(!has(&sim.log(), "systemctl restart"), "{:?}", sim.log());
}

#[test]
fn a_tray_that_cannot_be_put_back_does_not_keep_the_restored_agent_from_starting() {
    let sim = ready_to_install(Os::Linux);
    // once the new agent is in place: the prepared new tray goes away (so it cannot be put in
    // place) and so does the aside copy of the old tray (so it cannot be put back either)
    let (binary, tray) = (sim.layout.binary.clone(), sim.layout.tray.clone());
    *sim.on_sleep.borrow_mut() = Some(Box::new(move |_| {
        if std::fs::read(&binary).is_ok_and(|b| b == NEW_AGENT) {
            let name = tray.file_name().unwrap().to_string_lossy().into_owned();
            let _ = std::fs::remove_file(leftover_of(&tray));
            let _ = std::fs::remove_file(tray.with_file_name(format!(".{name}.old")));
        }
    }));
    let e = error(staged(&sim));
    let text = e.to_string();
    assert_eq!(e.code(), "rolled_back", "{text}");
    assert!(
        text.contains("cannot put the new printpapi-tray in place"),
        "{text}"
    );
    assert!(
        text.contains("printpapi-tray could not be put back"),
        "{text}"
    );
    assert!(!text.contains("The agent is not running"), "{text}");
    // what is left to do is said: the tray and the settings window are gone until then
    assert!(
        text.ends_with("It is running again. Run the setup again to put the tray program back."),
        "{text}"
    );
    // the old agent is back and was started again
    assert_eq!(sim.binary(), OLD_AGENT);
    let starts = sim
        .log()
        .iter()
        .filter(|l| l.contains("systemctl restart"))
        .count();
    assert_eq!(
        starts,
        2,
        "the new agent, then the old one: {:?}",
        sim.log()
    );
}

#[test]
fn the_next_update_puts_a_missing_tray_back_from_its_aside_copy_before_it_clears_leftovers() {
    // a tray that could not be put back leaves its only copy in .printpapi-tray.old and no tray:
    // the next update must not delete that copy and then finish without a tray
    let sim = Sim::new(Os::Linux);
    let tray = sim.layout.tray.clone();
    let name = tray.file_name().unwrap().to_string_lossy().into_owned();
    let aside = tray.with_file_name(format!(".{name}.old"));
    std::fs::rename(&tray, &aside).unwrap();
    sim.stage(&sim.release("3.7.0", NEW_AGENT, Some(NEW_TRAY)));
    staged(&sim).unwrap();
    assert_eq!(sim.binary(), NEW_AGENT);
    assert_eq!(sim.tray(), OLD_TRAY, "the old tray is back, not gone");
    assert!(!aside.exists());
}

// ---- tries and the unattended run ----

#[test]
fn failures_after_the_stop_count_per_version_and_an_unattended_run_gives_up_after_two() {
    let sim = Sim::new(Os::Linux);
    sim.new_agent.set(NewAgent::NeverOnline);
    let release = sim.good_release();
    sim.serve(DEFAULT_BASE, &release);
    assert_eq!(code(fetched(&sim)), "rolled_back");
    assert_eq!(state(&sim).tries, 1);
    assert_eq!(code(fetched(&sim)), "rolled_back");
    assert_eq!(state(&sim).tries, 2);
    // the third night: left alone, and only the signed file was asked for
    sim.downloaded.borrow_mut().clear();
    sim.log.borrow_mut().clear();
    assert_eq!(fetched(&sim).unwrap(), None);
    assert_eq!(
        *sim.downloaded.borrow(),
        [format!("{DEFAULT_BASE}/{MANIFEST_NAME}")]
    );
    nothing_stopped_or_started(&sim);
    // a click still works, and works even though the unattended runs gave up
    sim.new_agent.set(NewAgent::Good);
    sim.stage(&release);
    assert_eq!(staged(&sim).unwrap(), Some(Version(3, 7, 0)));
    assert_eq!(state(&sim).tries, 0);
}

#[test]
fn the_next_version_starts_at_zero_tries() {
    let sim = Sim::new(Os::Linux);
    sim.new_agent.set(NewAgent::NeverOnline);
    sim.serve(DEFAULT_BASE, &sim.good_release());
    assert_eq!(code(fetched(&sim)), "rolled_back");
    assert_eq!(code(fetched(&sim)), "rolled_back");
    // 3.8.0 is a new version: tried again, and its failure counts as the first
    let next = sim.release("3.8.0", b"agent 3.8.0", Some(b"tray 3.8.0"));
    sim.downloads.borrow_mut().clear();
    sim.serve(DEFAULT_BASE, &next);
    assert_eq!(code(fetched(&sim)), "rolled_back");
    let st = state(&sim);
    assert_eq!((st.version.as_str(), st.tries), ("3.8.0", 1));
}

#[test]
fn an_unattended_run_updates_when_everything_is_in_order() {
    let sim = Sim::new(Os::Linux);
    sim.serve(DEFAULT_BASE, &sim.good_release());
    assert_eq!(fetched(&sim).unwrap(), Some(Version(3, 7, 0)));
    assert_eq!(sim.binary(), NEW_AGENT);
    assert_eq!(sim.tray(), NEW_TRAY);
    assert_eq!(sim.downloaded.borrow().len(), 3);
}

#[test]
fn switching_updates_off_during_the_download_stops_the_unattended_install() {
    for change in [
        "updates = off",
        "updates = auto\nupdate_url = https://other.example/x",
    ] {
        let sim = Sim::new(Os::Linux);
        sim.serve(DEFAULT_BASE, &sim.good_release());
        let (ini, text) = (
            sim.layout.ini.clone(),
            INI.replace("updates = auto", change),
        );
        *sim.on_download.borrow_mut() =
            Some(Box::new(move || std::fs::write(&ini, &text).unwrap()));
        assert_eq!(fetched(&sim).unwrap(), None, "{change}");
        assert_eq!(sim.binary(), OLD_AGENT, "{change}");
        assert_eq!(sim.tray(), OLD_TRAY, "{change}");
        nothing_stopped_or_started(&sim);
    }
}

#[test]
fn an_unattended_run_uses_the_mirror_of_agent_ini() {
    let sim = Sim::new(Os::Linux);
    let ini = INI.replace(
        "updates = auto",
        "updates = auto\nupdate_url = https://mirror.example/printpapi/",
    );
    std::fs::write(&sim.layout.ini, ini).unwrap();
    sim.serve("https://mirror.example/printpapi", &sim.good_release());
    assert_eq!(fetched(&sim).unwrap(), Some(Version(3, 7, 0)));
    assert!(
        sim.downloaded
            .borrow()
            .iter()
            .all(|u| u.starts_with("https://mirror.example/printpapi/"))
    );
}

#[test]
fn an_unattended_run_is_silent_where_there_is_nothing_to_do() {
    let quiet = |sim: &Sim| {
        assert_eq!(fetched(sim).unwrap(), None);
        nothing_stopped_or_started(sim);
    };
    // notify, off and a typo: no request at all
    for mode in ["updates = notify", "updates = off", "updates = sometimes"] {
        let sim = Sim::new(Os::Linux);
        std::fs::write(&sim.layout.ini, INI.replace("updates = auto", mode)).unwrap();
        sim.serve(DEFAULT_BASE, &sim.good_release());
        quiet(&sim);
        assert!(sim.downloaded.borrow().is_empty(), "{mode}");
    }
    // a build without a key, a folder that is no folder
    let mut sim = Sim::new(Os::Linux);
    sim.keys.clear();
    sim.serve(DEFAULT_BASE, &sim.good_release());
    quiet(&sim);
    assert!(sim.downloaded.borrow().is_empty());
    let sim = Sim::new(Os::Linux);
    std::fs::write(
        &sim.layout.ini,
        INI.replace("updates = auto", "updates = auto\nupdate_url = ftp://x/y"),
    )
    .unwrap();
    quiet(&sim);
    assert!(sim.downloaded.borrow().is_empty());
    // an agent that is not connected is not updated, and nothing is asked
    let sim = Sim::new(Os::Linux);
    sim.no_status.set(true);
    sim.serve(DEFAULT_BASE, &sim.good_release());
    quiet(&sim);
    assert!(sim.downloaded.borrow().is_empty());
    // nothing published yet (404), and a version that is not newer
    let sim = Sim::new(Os::Linux);
    quiet(&sim);
    assert_eq!(sim.downloaded.borrow().len(), 1);
    let sim = Sim::new(Os::Linux);
    sim.serve(
        DEFAULT_BASE,
        &sim.release("3.6.0", NEW_AGENT, Some(NEW_TRAY)),
    );
    quiet(&sim);
    assert_eq!(sim.downloaded.borrow().len(), 1, "only the signed file");
}

#[test]
fn an_unattended_run_says_so_when_the_network_or_the_signature_fails() {
    let sim = Sim::new(Os::Linux);
    sim.downloads.borrow_mut().push((
        format!("{DEFAULT_BASE}/{MANIFEST_NAME}"),
        Err(DownloadError::Other("connection failed: refused".into())),
    ));
    assert_eq!(code(fetched(&sim)), "unreachable");
    let mut sim = Sim::new(Os::Linux);
    sim.serve(DEFAULT_BASE, &sim.good_release());
    sim.keys = vec![[9; 32]];
    assert_eq!(code(fetched(&sim)), "bad_signature");
    // a file that is gone between the signed file and the file: try again later
    let sim = Sim::new(Os::Linux);
    let release = sim.good_release();
    sim.serve(DEFAULT_BASE, &release[..2]);
    let e = error(fetched(&sim));
    assert_eq!(e.code(), "bad_file");
    assert!(e.to_string().contains("Try again in a few minutes"));
    nothing_stopped_or_started(&sim);
}

#[test]
fn an_unattended_run_installs_nothing_when_the_binary_changed_since_it_started() {
    let sim = Sim::new(Os::Linux);
    sim.serve(DEFAULT_BASE, &sim.good_release());
    let binary = sim.layout.binary.clone();
    *sim.on_download.borrow_mut() = Some(Box::new(move || {
        std::fs::write(&binary, b"agent 3.6.9 by a setup").unwrap();
    }));
    assert_eq!(fetched(&sim).unwrap(), None);
    nothing_stopped_or_started(&sim);
    assert_eq!(
        std::fs::read(&sim.layout.binary).unwrap(),
        b"agent 3.6.9 by a setup"
    );
    assert!(!state_path(&sim.layout).exists());
}

// ---- no secrets anywhere ----

#[test]
fn neither_the_key_nor_a_header_value_reaches_the_state_a_message_a_command_or_the_network() {
    let sim = Sim::new(Os::Linux);
    sim.new_agent.set(NewAgent::NeverOnline);
    sim.serve(DEFAULT_BASE, &sim.good_release());
    crate::ERR_LINES.with(|l| l.borrow_mut().clear());
    let e = error(fetched(&sim));
    let mut everything = vec![
        e.to_string(),
        std::fs::read_to_string(state_path(&sim.layout)).unwrap(),
    ];
    everything.extend(sim.log());
    everything.extend(sim.downloaded.borrow().iter().cloned());
    everything.extend(crate::ERR_LINES.with(|l| l.borrow().clone()));
    for text in everything {
        assert!(!text.contains("SECRET-KEY-VALUE"), "{text}");
        assert!(!text.contains("HEADER-SECRET-VALUE"), "{text}");
    }
}

// ---- the pieces ----

#[test]
fn an_existing_private_folder_is_trusted_only_when_it_is_closed() {
    let path = Path::new("/opt/printpapi-agent/.update");
    let ok = DirFacts {
        is_dir: true,
        is_link: false,
        uid: Some(0),
        mode: Some(0o700),
    };
    assert!(check_private_dir(&ok, true, path).is_ok());
    // the owner is only asked of root
    assert!(
        check_private_dir(
            &DirFacts {
                uid: Some(1000),
                ..ok
            },
            false,
            path
        )
        .is_ok()
    );
    for bad in [
        DirFacts {
            is_link: true,
            ..ok
        },
        DirFacts {
            is_dir: false,
            ..ok
        },
        DirFacts {
            uid: Some(1000),
            ..ok
        },
        DirFacts {
            mode: Some(0o750),
            ..ok
        },
        DirFacts {
            mode: Some(0o707),
            ..ok
        },
        DirFacts {
            mode: Some(0o755),
            ..ok
        },
    ] {
        let e = check_private_dir(&bad, true, path).unwrap_err();
        assert_eq!(e.code(), "system", "{bad:?}");
        assert!(!e.to_string().contains("  "), "{e}");
    }
    // Windows has no owner or mode to look at: a real folder is enough
    let windows = DirFacts {
        is_dir: true,
        is_link: false,
        uid: None,
        mode: None,
    };
    assert!(check_private_dir(&windows, true, path).is_ok());
}

#[test]
fn a_lock_that_is_a_link_or_a_folder_is_refused() {
    let sim = Sim::new(Os::Linux);
    std::fs::remove_file(lock_path(&sim.layout)).unwrap();
    std::fs::create_dir(lock_path(&sim.layout)).unwrap();
    let e = lock_if_exists(&sim.env()).unwrap_err();
    assert_eq!(e.code(), "system");
}

#[test]
fn leftovers_are_named_new_failed_and_old_to_old9() {
    let names: Vec<String> = leftovers_of(Path::new("/opt/p/printpapi-agent"))
        .iter()
        .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
        .collect();
    assert_eq!(names.len(), 12);
    for want in [
        ".printpapi-agent.new",
        ".printpapi-agent.failed",
        ".printpapi-agent.old",
        ".printpapi-agent.old1",
        ".printpapi-agent.old9",
    ] {
        assert!(names.contains(&want.to_string()), "{want}");
    }
}

#[test]
fn the_status_file_is_read_only_as_a_small_regular_file() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("status.json");
    assert!(read_status_file(&path).is_none());
    std::fs::write(&path, r#"{"state":"online"}"#).unwrap();
    let seen = read_status_file(&path).unwrap();
    assert!(seen.age < 60.0 && seen.age >= 0.0);
    assert!(seen.text.contains("online"));
    std::fs::write(&path, vec![b' '; 64 * 1024 + 1]).unwrap();
    assert!(read_status_file(&path).is_none(), "over 64 KB");
    std::fs::remove_file(&path).unwrap();
    std::fs::create_dir(&path).unwrap();
    assert!(read_status_file(&path).is_none(), "a folder");
}

#[test]
fn the_state_file_round_trips_without_secrets() {
    let sim = Sim::new(Os::Linux);
    write_state(
        &sim.env(),
        &State {
            version: "3.7.0".into(),
            from: "3.6.0".into(),
            at: 12.5,
            ok: false,
            tries: 2,
            message: "x".repeat(2000),
        },
        &"ä".repeat(2000),
    );
    let st = read_state(&sim.layout).unwrap();
    assert_eq!((st.version.as_str(), st.tries, st.ok), ("3.7.0", 2, false));
    assert_eq!(st.message.chars().count(), 800);
    assert_eq!(
        state_json(&sim)["detail"].as_str().unwrap().chars().count(),
        400
    );
    assert!(read_state(&Sim::new(Os::Linux).layout).is_none());
}

#[cfg(windows)]
#[test]
fn a_running_exe_can_be_moved_aside_replaced_and_put_back_and_its_old_file_goes_at_reboot() {
    // a real running program: a copy of ping.exe that waits for 30 seconds
    let sim = Sim::new(Os::Windows);
    let ping = Path::new(r"C:\Windows\System32\PING.EXE");
    if !ping.exists() {
        return;
    }
    let to = sim.layout.dir.join("running.exe");
    std::fs::copy(ping, &to).unwrap();
    let mut child = std::process::Command::new(&to)
        .args(["-n", "30", "127.0.0.1"])
        .stdout(std::process::Stdio::null())
        .spawn()
        .unwrap();
    std::fs::write(leftover_of(&to), b"the new program").unwrap();
    let env = sim.env();
    let s = swap(&env, &to).expect("rename works while the program runs");
    assert_eq!(std::fs::read(&to).unwrap(), b"the new program");
    assert!(s.old.ends_with(".running.exe.old"));
    // a locked .old makes the next swap take .old1
    std::fs::write(leftover_of(&to), b"newer").unwrap();
    let s2 = swap(&env, &to).unwrap();
    assert!(
        s2.old.ends_with(".running.exe.old1"),
        "{}",
        s2.old.display()
    );
    // the running exe cannot be deleted: it goes at the next restart
    discard(&env, &s.old);
    assert_eq!(*sim.at_reboot.borrow(), std::slice::from_ref(&s.old));
    // and the swap can be undone while it runs
    std::fs::rename(&to, to.with_file_name("failed")).unwrap();
    std::fs::rename(&s2.old, &to).unwrap();
    child.kill().unwrap();
    child.wait().unwrap();
    discard(&env, &s.old);
    assert!(!s.old.exists());
}
