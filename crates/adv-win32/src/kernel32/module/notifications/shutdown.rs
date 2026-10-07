//! Loader shutdown is once per process and returns to its caller. Process and
//! peer termination remain responsibilities of the terminal ExitProcess owner.
use super::*;

#[derive(Default)]
struct ProcessShutdown {
    started: bool,
}

pub(crate) fn shutdown_process(env: &mut Env<'_>) -> Outcome {
    if std::mem::replace(
        &mut env.ext.get_or_default::<ProcessShutdown>().started,
        true,
    ) {
        return Outcome::Return(0);
    }
    // Native authored probes observe current-thread FLS before DLL detach,
    // while live peers retain their own FLS and continue running afterwards.
    super::super::super::sync::detach_fls(
        env,
        Box::new(move |env| {
            terminal_notifications(
                env,
                0,
                Box::new(move |env| {
                    match crate::msvcrt::detach_process(env)
                        .and_then(|()| crate::gdi32::font::detach_process(env))
                    {
                        Ok(()) => Outcome::Return(0),
                        Err(error) => Outcome::Fail(error),
                    }
                }),
            )
        }),
    )
}
