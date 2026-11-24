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

    /// Imports a `.reg` file into this host's Wine prefix (WINE-SES-11).
    pub fn import_registry(
        &mut self,
        config: &Config<'_>,
        file: &Path,
    ) -> Result<registry::RegistryReport, String> {
        registry::import_reg(self, config, file)
    }

    /// The registry a script is about to change, when `options` start one.
    pub fn watch_registry(
        &mut self,
        config: &Config<'_>,
        options: &SessionOptions,
    ) -> Result<Option<registry::RegistryWatch>, String> {
        let script = match options.program.as_ref().map(|program| &program.image) {
            Some(Image::Host(path)) => registry::is_script(path),
            Some(Image::Guest(path)) => registry::is_script(Path::new(path)),
            None => false,
        };
        if script {
            registry::watch(self, config).map(Some)
        } else {
            Ok(None)
        }
    }

    /// What a script wrote since `watch`, its `WOW6432Node` keys mirrored to
    /// the 32-bit `SOFTWARE` tree (WINE-SES-11).
    pub fn settle_registry(
        &mut self,
        config: &Config<'_>,
        watch: &registry::RegistryWatch,
    ) -> Result<registry::RegistryReport, String> {
        registry::settle(self, config, watch)
    }

    /// The programs setups registered on this host's prefix, where they lie
    /// on the host (WINE-SES-14).
    pub fn installed(
        &mut self,
        config: &Config<'_>,
        options: &SessionOptions,
    ) -> Result<Vec<super::Installed>, String> {
        super::installed::installed(self, config, options)
    }

    /// As Wine's `run_wineboot`, but restarting: the template stands for
    /// what `--init` installs, and no services run. Once per host. The boot
    /// shares the caller's modules and audio, so it holds no second copy.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn boot(
        &mut self,
        config: &Config<'_>,
        prefix: &Path,
        drive: &Path,
        socket: &Path,
        clock: &mut adv_machine::Clock,
        wake: &WakeEvent,
        shared: &Shared,
        mounts: &adv_nt::file::volume::Mounts,
        (ntdll, apisets): (&[u8], &[u8]),
        audio: &adv_device::AudioControl,
    ) -> Result<(), String> {
        if self.booted() {
            return Ok(());
        }
        let process_config = Config {
            fonts: "",
            ..*config
        };
        let mut boot = new_machine(&process_config, drive, clock.clone(), wake, audio)?;
        boot.cpu.executor_mut().use_tier1_only();
        boot.ext.insert(mounts.clone());
        let command_line = format!("\"{WINEBOOT}\" --restart");
        let startup = Startup {
            image: WINEBOOT,
            command_line: &command_line,
            directory: r"C:\windows\system32\",
            environment: ENVIRONMENT,
            locale: config.locale,
            debug: config.debug,
            stdin: None,
            stdout: None,
            stderr: None,
            graphics_driver: super::super::GRAPHICS_DRIVER,
            fonts: "",
        };
        process::start(
            &mut boot,
            ntdll,
            apisets,
            &startup,
            prefix,
            socket,
            Box::new(shared.clone()),
        )
        .map_err(|e| format!("wineboot: {e}"))?;
        boot.ext.insert(shared.clone());
        adv_nt::identity::note_ntdll(&mut boot, &config.dist.join(NTDLL), ntdll);
        match run(
            &mut boot,
            wake,
            Instant::now() + BOOT,
            |_| Ok(false),
            None,
            None,
        ) {
            Stop::Exited(0) => {}
            stop => return Err(format!("wineboot: {stop:?}")),
        }
        drop(boot);
        self.finish_boot();
        Ok(())
    }

    /// Runs `work` against the booted prefix server while no session runs:
    /// its socket, the connection owner and the time of the call.
    pub(super) fn serve<R>(
        &mut self,
        config: &Config<'_>,
        work: impl FnOnce(&Path, &mut Shared, Time) -> Result<R, String>,
    ) -> Result<R, String> {
        self.check(config)?;
        let (prefix, drive) = self.prefix(config)?;
        let wake = WakeEvent::new().map_err(|e| e.to_string())?;
        let mut clock = adv_machine::Clock::real();
        let mounts = adv_nt::file::volume::Mounts::default();
        let mut shared = self.connect(config, &mut clock, &wake, mounts.clone())?;
        let _lease = Lease(shared.clone());
        let socket = shared
            .driver
            .borrow()
            .server
            .socket_dir()
            .ok_or("the server has no socket")?;
        if !self.booted() {
            let module = |name: &str| {
                let path = config.dist.join(MODULES).join(name);
                std::fs::read(&path).map_err(|e| format!("{}: {e}", path.display()))
            };
            let (ntdll, apisets) = (module("ntdll.dll")?, module("apisetschema.dll")?);
            let audio = adv_device::AudioControl::new(true);
            self.boot(
                config,
                &prefix,
                &drive,
                &socket,
                &mut clock,
                &wake,
                &shared,
                &mounts,
                (&ntdll, &apisets),
                &audio,
            )?;
        }
        work(&socket, &mut shared, Time::now(&mut clock))
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
