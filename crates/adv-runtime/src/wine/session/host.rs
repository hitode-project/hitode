//! One prefix server per execution owner; guest machines last only one session.
use super::*;

/// The Wine environment of an execution owner. A host runs sessions sequentially
/// against one installation and prefix, without restarting Wine's global server.
#[derive(Default)]
pub struct Host {
    binding: Option<Binding>,
    pub(super) fonts: fonts::Catalog,
}

struct Binding {
    prefix: PathBuf,
    dist: PathBuf,
    drive: PathBuf,
    driver: Rc<RefCell<Driver>>,
    booted: bool,
}

impl Host {
    pub(super) fn prefix(&mut self, config: &Config<'_>) -> Result<(PathBuf, PathBuf), String> {
        let drive = config
            .drive
            .map_or_else(|| config.prefix.join("drive_c"), Path::to_owned);
        if self
            .binding
            .as_ref()
            .is_some_and(|binding| binding.drive == drive)
        {
            Ok((config.prefix.to_owned(), drive))
        } else {
            let prepared = prefix(config)?;
            if let Some(binding) = &mut self.binding {
                binding.drive = drive;
            }
            Ok(prepared)
        }
    }

    pub(super) fn booted(&self) -> bool {
        self.binding.as_ref().is_some_and(|binding| binding.booted)
    }

    pub(super) fn finish_boot(&mut self) {
        self.binding.as_mut().unwrap().booted = true;
    }

    pub fn start(
        &mut self,
        config: &Config<'_>,
        wake: WakeEvent,
        options: &SessionOptions,
        prepare: impl FnOnce(&mut Machine<Cpu>) -> Result<(), String>,
    ) -> Result<Session, String> {
        Session::start_on(self, config, wake, options, prepare)
    }

    pub(super) fn check(&self, config: &Config<'_>) -> Result<(), String> {
        if let Some(binding) = &self.binding {
            if binding.prefix != config.prefix || binding.dist != config.dist {
                return Err("a Wine host uses one prefix and installation".into());
            }
            if Rc::strong_count(&binding.driver) != 1 {
                return Err("a Wine session is already running".into());
            }
        }
        Ok(())
    }

    pub(super) fn connect(
        &mut self,
        config: &Config<'_>,
        clock: &mut adv_machine::Clock,
        wake: &WakeEvent,
        mounts: adv_nt::file::volume::Mounts,
    ) -> Result<Shared, String> {
        if self.binding.is_none() {
            let time = GuestClock(Cell::new(Time::now(clock)));
            let mut server = Server::start(
                &adv_wineserver::Config {
                    prefix: config.prefix.to_owned(),
                    server_dir: config.prefix.join("server"),
                    install: config.dist.to_owned(),
                    debug: 0,
                },
                &time,
            )
            .map_err(|e| e.to_string())?;
            server.keep_alive();
            let input = server.input_generation();
            self.binding = Some(Binding {
                prefix: config.prefix.to_owned(),
                dist: config.dist.to_owned(),
                drive: config
                    .drive
                    .map_or_else(|| config.prefix.join("drive_c"), Path::to_owned),
                booted: false,
                driver: Rc::new(RefCell::new(Driver {
                    server,
                    clock: time,
                    parked: BTreeMap::new(),
                    signals: Vec::new(),
                    generation: 0,
                    deadline: None,
                    guest_deadline: None,
                    input,
                    mounts: Default::default(),
                    #[cfg(test)]
                    resumes: 0,
                })),
            });
        }
        let driver = self.binding.as_ref().unwrap().driver.clone();
        {
            let mut driver = driver.borrow_mut();
            // A new game's clock starts beyond the last server observation.
            clock.epoch_us = driver.clock.monotonic() / 10 + 1;
            driver.clock.0.set(Time::now(clock));
            driver.mounts = mounts;
            let wake = wake.clone();
            driver.server.set_waker(Box::new(move || wake.signal()));
            let delay = match driver.server.state() {
                State::Waiting(delay) => delay,
                State::Exited(code) => return Err(format!("Wine server exited with {code}")),
            };
            driver.deadline = delay.map(|delay| Instant::now() + delay);
            driver.guest_deadline =
                delay.map(|delay| driver.clock.monotonic() + delay.as_nanos() as u64 / 100);
        }
        Ok(Shared {
            driver,
            id: Rc::new(Cell::new(0)),
        })
    }
}

pub(super) struct Lease(pub Shared);

impl Drop for Lease {
    fn drop(&mut self) {
        // Machines hold Shared too; drop them outside the driver's borrow.
        let parked = std::mem::take(&mut self.0.driver.borrow_mut().parked);
        drop(parked);
        let mut driver = self.0.driver.borrow_mut();
        let Driver { server, clock, .. } = &mut *driver;
        server.flush(clock);
        server.set_waker(Box::new(|| {}));
        driver.signals.clear();
        driver.mounts = Default::default();
    }
}
