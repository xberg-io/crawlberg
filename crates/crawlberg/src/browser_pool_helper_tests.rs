//! Readiness of the owned helper used by the pool teardown fixture.

use std::io;
use std::path::{Path, PathBuf};
use std::time::Instant;

use super::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Candidate {
    process: LaunchedProcess,
    named: bool,
}

#[derive(Clone, Copy)]
enum Readiness {
    Gone,
    Waiting,
    Stopped,
}

trait Setup {
    fn candidate(&mut self) -> io::Result<Option<Candidate>>;
    fn stop(&mut self, candidate: Candidate) -> io::Result<bool>;
    fn observe(&mut self, candidate: Candidate) -> io::Result<Readiness>;
}

fn prepare(setup: &mut impl Setup, deadline: Instant) -> io::Result<(Candidate, bool)> {
    loop {
        remaining(deadline)?;
        let Some(candidate) = setup.candidate()? else {
            pause(deadline)?;
            continue;
        };
        remaining(deadline)?;
        let stopped = match setup.stop(candidate) {
            Ok(stopped) => stopped,
            Err(error) if error.raw_os_error() == Some(libc::ESRCH) => false,
            Err(error) => return Err(error),
        };
        if !stopped {
            pause(deadline)?;
            continue;
        }
        loop {
            remaining(deadline)?;
            match setup.observe(candidate)? {
                Readiness::Stopped => {
                    remaining(deadline)?;
                    return Ok((candidate, stopped));
                }
                Readiness::Gone => break,
                Readiness::Waiting => pause(deadline)?,
            }
        }
    }
}

fn remaining(deadline: Instant) -> io::Result<Duration> {
    deadline
        .checked_duration_since(Instant::now())
        .filter(|duration| !duration.is_zero())
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::TimedOut,
                "the owned helper did not become stopped within the fixture budget",
            )
        })
}

fn pause(deadline: Instant) -> io::Result<()> {
    std::thread::sleep(PROFILE_USERS_POLL_INTERVAL.min(remaining(deadline)?));
    Ok(())
}

pub(super) fn stopped_helper(path: &Path) -> io::Result<(Pid, bool, bool)> {
    let deadline = Instant::now() + PROCESS_TEST_WAIT;
    let (candidate, stopped) = prepare(&mut Family::new(path)?, deadline)?;
    Ok((Pid::from_u32(candidate.process.pid), candidate.named, stopped))
}

struct Family {
    main: LaunchedProcess,
    executable: PathBuf,
    flag: String,
}

impl Family {
    fn new(path: &Path) -> io::Result<Self> {
        let flag = user_data_dir_flag(path);
        let mut system = System::new().map_err(io::Error::other)?;
        let main = processes_naming(&mut system, &flag)
            .into_iter()
            .find(|process| {
                !process
                    .cmd()
                    .iter()
                    .any(|argument| argument.to_string_lossy().contains("--type="))
            })
            .ok_or_else(|| io::Error::other("the pool's Chrome must have a main process"))?;
        Ok(Self {
            main: identity(main.pid())?.ok_or_else(|| io::Error::other("the owned Chrome ended"))?,
            executable: main
                .exe()
                .ok_or_else(|| io::Error::other("the owned Chrome executable must be readable"))?
                .to_path_buf(),
            flag,
        })
    }

    fn snapshot(&self) -> io::Result<System> {
        let mut system = System::new().map_err(io::Error::other)?;
        system.refresh_processes_specifics(ProcessesToUpdate::All, true, user_refresh());
        let main = system
            .process(Pid::from_u32(self.main.pid))
            .ok_or_else(|| io::Error::other("the owned Chrome is no longer inspectable"))?;
        if identity(main.pid())? != Some(self.main)
            || main.exe() != Some(self.executable.as_path())
            || !names(main, &self.flag)
        {
            return Err(io::Error::other("the owned Chrome identity changed"));
        }
        Ok(system)
    }

    fn checked(&self, candidate: Candidate) -> io::Result<Option<System>> {
        if identity(Pid::from_u32(candidate.process.pid))? != Some(candidate.process) {
            return Ok(None);
        }
        let system = self.snapshot()?;
        let pid = Pid::from_u32(candidate.process.pid);
        if system.process(pid).is_none() {
            if gone(pid)? {
                return Ok(None);
            }
            return Err(io::Error::other("the owned helper exists but is not inspectable"));
        }
        if !descends_from(&system, pid, Pid::from_u32(self.main.pid)) {
            return Err(io::Error::other("the helper no longer belongs to the owned Chrome"));
        }
        if identity(pid)? != Some(candidate.process) {
            return Ok(None);
        }
        Ok(Some(system))
    }
}

impl Setup for Family {
    fn candidate(&mut self) -> io::Result<Option<Candidate>> {
        let system = self.snapshot()?;
        let main = Pid::from_u32(self.main.pid);
        let helpers: Vec<_> = system
            .processes()
            .values()
            .filter(|process| {
                process.pid() != main
                    && process.status() != ProcessStatus::Zombie
                    && descends_from(&system, process.pid(), main)
            })
            .collect();
        let helper = helpers
            .iter()
            .copied()
            .find(|process| !names(process, &self.flag))
            .or_else(|| helpers.first().copied());
        let Some(helper) = helper else {
            return Ok(None);
        };
        Ok(identity(helper.pid())?.map(|process| Candidate {
            process,
            named: names(helper, &self.flag),
        }))
    }

    fn stop(&mut self, candidate: Candidate) -> io::Result<bool> {
        let Some(system) = self.checked(candidate)? else {
            return Ok(false);
        };
        let process = system
            .process(Pid::from_u32(candidate.process.pid))
            .expect("the checked helper exists");
        match process.kill_with(Signal::Stop) {
            Some(true) => Ok(true),
            Some(false) => Err(io::Error::last_os_error()),
            None => Err(io::Error::new(io::ErrorKind::Unsupported, "SIGSTOP is unsupported")),
        }
    }

    fn observe(&mut self, candidate: Candidate) -> io::Result<Readiness> {
        let Some(system) = self.checked(candidate)? else {
            return Ok(Readiness::Gone);
        };
        Ok(
            if system
                .process(Pid::from_u32(candidate.process.pid))
                .expect("the checked helper exists")
                .status()
                == ProcessStatus::Stop
            {
                Readiness::Stopped
            } else {
                Readiness::Waiting
            },
        )
    }
}

fn descends_from(system: &System, mut pid: Pid, main: Pid) -> bool {
    for _ in 0..system.processes().len() {
        let Some(parent) = system.process(pid).and_then(Process::parent) else {
            return false;
        };
        if parent == main {
            return true;
        }
        pid = parent;
    }
    false
}

fn identity(pid: Pid) -> io::Result<Option<LaunchedProcess>> {
    if let Some(process) = LaunchedProcess::of(pid.as_u32()) {
        return Ok(Some(process));
    }
    if gone(pid)? {
        Ok(None)
    } else {
        Err(io::Error::other("a live owned process has no readable start identity"))
    }
}

#[allow(
    unsafe_code,
    reason = "signal zero checks existence of an already observed owned process"
)]
fn gone(pid: Pid) -> io::Result<bool> {
    // SAFETY: signal zero sends no signal and reads no process arguments or environment. ~keep
    if unsafe { libc::kill(pid.as_u32() as libc::pid_t, 0) } == 0 {
        return Ok(false);
    }
    let error = io::Error::last_os_error();
    if error.raw_os_error() == Some(libc::ESRCH) {
        Ok(true)
    } else {
        Err(error)
    }
}

#[cfg(test)]
mod readiness {
    use std::collections::VecDeque;

    use super::*;

    struct Scripted {
        candidates: VecDeque<Option<Candidate>>,
        signals: VecDeque<io::Result<bool>>,
        observations: VecDeque<io::Result<Readiness>>,
        selected: usize,
        signaled: usize,
        observed: usize,
    }

    impl Setup for Scripted {
        fn candidate(&mut self) -> io::Result<Option<Candidate>> {
            self.selected += 1;
            Ok(self.candidates.pop_front().flatten())
        }

        fn stop(&mut self, _: Candidate) -> io::Result<bool> {
            self.signaled += 1;
            self.signals
                .pop_front()
                .expect("the fixture provides every signal result")
        }

        fn observe(&mut self, _: Candidate) -> io::Result<Readiness> {
            self.observed += 1;
            self.observations
                .pop_front()
                .expect("the fixture provides every observation")
        }
    }

    fn candidate(pid: u32) -> Candidate {
        Candidate {
            process: LaunchedProcess { pid, started: 1 },
            named: false,
        }
    }

    fn scripted(signals: Vec<io::Result<bool>>, states: Vec<Readiness>) -> Scripted {
        Scripted {
            candidates: [Some(candidate(1)), Some(candidate(2))].into(),
            signals: signals.into(),
            observations: states.into_iter().map(Ok).collect(),
            selected: 0,
            signaled: 0,
            observed: 0,
        }
    }

    #[test]
    fn retries_a_candidate_that_vanished_at_the_signal_boundary() {
        let mut setup = scripted(
            vec![Err(io::Error::from_raw_os_error(libc::ESRCH)), Ok(true)],
            vec![Readiness::Stopped],
        );
        let (ready, sent) = prepare(&mut setup, Instant::now() + PROCESS_TEST_WAIT).expect("a survivor becomes ready");
        assert_eq!((ready, sent), (candidate(2), true));
        assert_eq!((setup.selected, setup.signaled, setup.observed), (2, 2, 1));
    }

    #[test]
    fn waits_for_observed_stop_after_a_successful_signal() {
        let mut setup = scripted(vec![Ok(true)], vec![Readiness::Waiting, Readiness::Stopped]);
        let (ready, sent) = prepare(&mut setup, Instant::now() + PROCESS_TEST_WAIT).expect("the stop is observed");
        assert_eq!((ready, sent), (candidate(1), true));
        assert_eq!((setup.selected, setup.signaled, setup.observed), (1, 1, 2));
    }

    #[test]
    fn retries_a_candidate_that_disappeared_before_the_ready_observation() {
        let mut setup = scripted(vec![Ok(true), Ok(true)], vec![Readiness::Gone, Readiness::Stopped]);
        let (ready, sent) = prepare(&mut setup, Instant::now() + PROCESS_TEST_WAIT).expect("a survivor becomes ready");
        assert_eq!((ready, sent), (candidate(2), true));
        assert_eq!((setup.selected, setup.signaled, setup.observed), (2, 2, 2));
    }

    #[test]
    fn never_retries_permission_or_unexplained_signal_errors() {
        for errno in [libc::EPERM, libc::EACCES, libc::EIO, libc::EINTR] {
            let mut setup = scripted(vec![Err(io::Error::from_raw_os_error(errno))], vec![]);
            let error = prepare(&mut setup, Instant::now() + PROCESS_TEST_WAIT).expect_err("signal errors fail closed");
            assert_eq!(error.raw_os_error(), Some(errno));
            assert_eq!((setup.selected, setup.signaled, setup.observed), (1, 1, 0));
        }
    }

    #[test]
    fn never_retries_an_unsupported_signal() {
        let mut setup = scripted(
            vec![Err(io::Error::new(io::ErrorKind::Unsupported, "no SIGSTOP"))],
            vec![],
        );
        let error =
            prepare(&mut setup, Instant::now() + PROCESS_TEST_WAIT).expect_err("unsupported is not disappearance");
        assert_eq!(error.kind(), io::ErrorKind::Unsupported);
        assert_eq!((setup.selected, setup.signaled, setup.observed), (1, 1, 0));
    }

    #[test]
    fn retries_a_candidate_confirmed_gone_before_the_signal() {
        let mut setup = scripted(vec![Ok(false), Ok(true)], vec![Readiness::Stopped]);
        let (ready, sent) = prepare(&mut setup, Instant::now() + PROCESS_TEST_WAIT).expect("a survivor becomes ready");
        assert_eq!((ready, sent), (candidate(2), true));
        assert_eq!((setup.selected, setup.signaled, setup.observed), (2, 2, 1));
    }

    #[test]
    fn waits_for_initial_helper_discovery_without_sending_a_signal_to_nothing() {
        let mut setup = scripted(vec![Ok(true)], vec![Readiness::Stopped]);
        setup.candidates.push_front(None);
        let (ready, sent) = prepare(&mut setup, Instant::now() + PROCESS_TEST_WAIT).expect("the helper appears");
        assert_eq!((ready, sent), (candidate(1), true));
        assert_eq!((setup.selected, setup.signaled, setup.observed), (2, 1, 1));
    }

    #[test]
    fn never_retries_a_failed_readiness_inspection() {
        let mut setup = scripted(vec![Ok(true)], vec![]);
        setup
            .observations
            .push_back(Err(io::Error::from_raw_os_error(libc::EPERM)));
        let error = prepare(&mut setup, Instant::now() + PROCESS_TEST_WAIT).expect_err("inspection errors fail closed");
        assert_eq!(error.raw_os_error(), Some(libc::EPERM));
        assert_eq!((setup.selected, setup.signaled, setup.observed), (1, 1, 1));
    }

    #[test]
    fn an_expired_fixture_budget_performs_no_selection_or_signal() {
        let mut setup = scripted(vec![], vec![]);
        let error = prepare(&mut setup, Instant::now()).expect_err("an expired budget cannot become ready");
        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
        assert_eq!((setup.selected, setup.signaled, setup.observed), (0, 0, 0));
    }
}
