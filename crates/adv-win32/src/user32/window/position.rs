//! Position changes prepare all guest edits before publishing deferred geometry.
use super::*;
#[derive(Clone, Copy)]
struct Change {
    hwnd: u32,
    after: u32,
    rect: Rect,
    flags: u32,
}
fn rectangle(x: i32, y: i32, width: i32, height: i32) -> Option<Rect> {
    Some(Rect {
        left: x,
        top: y,
        right: x.checked_add(width.max(0))?,
        bottom: y.checked_add(height.max(0))?,
    })
}
pub(super) fn requested(
    env: &Env<'_>,
    hwnd: u32,
    x: i32,
    y: i32,
    width: i32,
    height: i32,
    flags: u32,
) -> Result<Rect, u32> {
    let window = user(env)
        .windows
        .get(&env.sys.objects, &hwnd)
        .ok_or(1400u32)?;
    let (x, y) = if flags & SWP_NOMOVE != 0 {
        (window.rect.left, window.rect.top)
    } else {
        (x, y)
    };
    let (width, height) = if flags & SWP_NOSIZE != 0 {
        (window.rect.width(), window.rect.height())
    } else {
        (width, height)
    };
    rectangle(x, y, width, height).ok_or(87)
}
struct Prepared {
    change: Change,
    parameter: u32,
}
impl Prepared {
    fn release(self, env: &mut Env<'_>) {
        temp_free(env, self.parameter);
    }
}
fn prepare(
    env: &mut Env<'_>,
    mut change: Change,
    then: impl FnOnce(&mut Env<'_>, Option<Prepared>) -> Outcome + 'static,
) -> Outcome {
    let u = user(env);
    let Some(window) = u.windows.get(&env.sys.objects, &change.hwnd) else {
        set_last_error(env, 1400);
        return then(env, None);
    };
    if change.flags & SWP_NOZORDER == 0
        && !z_order::valid(u, &env.sys.objects, change.hwnd, change.after)
    {
        set_last_error(env, 1400);
        return then(env, None);
    }
    let old = window.rect;
    let x = if change.flags & SWP_NOMOVE != 0 {
        old.left
    } else {
        change.rect.left
    };
    let y = if change.flags & SWP_NOMOVE != 0 {
        old.top
    } else {
        change.rect.top
    };
    let cx = if change.flags & SWP_NOSIZE != 0 {
        old.width()
    } else {
        change.rect.width().max(0)
    };
    let cy = if change.flags & SWP_NOSIZE != 0 {
        old.height()
    } else {
        change.rect.height().max(0)
    };
    if (x, y) == (old.left, old.top) && change.flags & SWP_FRAMECHANGED == 0 {
        change.flags |= SWP_NOMOVE | SWP_NOCLIENTMOVE;
    }
    if (cx, cy) == (old.width(), old.height()) && change.flags & SWP_FRAMECHANGED == 0 {
        change.flags |= SWP_NOSIZE | SWP_NOCLIENTSIZE;
    }
    let parameter = temp_alloc(env, 28);
    if parameter == 0 {
        set_last_error(env, 8);
        return then(env, None);
    }
    for (index, value) in [
        change.hwnd,
        change.after,
        x as u32,
        y as u32,
        cx as u32,
        cy as u32,
        change.flags,
    ]
    .into_iter()
    .enumerate()
    {
        if env
            .cpu
            .write_u32(parameter + index as u32 * 4, value)
            .is_err()
        {
            temp_free(env, parameter);
            set_last_error(env, 8);
            return then(env, None);
        }
    }
    let changing = move |env: &mut Env<'_>, _: u32| {
        let at = |i: u32| env.cpu.read_u32(parameter + i * 4).unwrap_or(0);
        let mut flags = at(6);
        let after = at(1);
        let u = user(env);
        let Some(window) = u.windows.get(&env.sys.objects, &change.hwnd) else {
            temp_free(env, parameter);
            set_last_error(env, 1400);
            return then(env, None);
        };
        if flags & SWP_NOZORDER == 0 && !z_order::valid(u, &env.sys.objects, change.hwnd, after) {
            temp_free(env, parameter);
            set_last_error(env, 1400);
            return then(env, None);
        }
        let old = window.rect;
        let x = if flags & SWP_NOMOVE != 0 {
            old.left
        } else {
            at(2) as i32
        };
        let y = if flags & SWP_NOMOVE != 0 {
            old.top
        } else {
            at(3) as i32
        };
        let cx = if flags & SWP_NOSIZE != 0 {
            old.width()
        } else {
            (at(4) as i32).max(0)
        };
        let cy = if flags & SWP_NOSIZE != 0 {
            old.height()
        } else {
            (at(5) as i32).max(0)
        };
        flags &= !(SWP_NOCLIENTMOVE | SWP_NOCLIENTSIZE);
        if (x, y) == (old.left, old.top) && flags & SWP_FRAMECHANGED == 0 {
            flags |= SWP_NOMOVE | SWP_NOCLIENTMOVE;
        }
        if (cx, cy) == (old.width(), old.height()) && flags & SWP_FRAMECHANGED == 0 {
            flags |= SWP_NOSIZE | SWP_NOCLIENTSIZE;
        }
        for (index, value) in [
            change.hwnd,
            after,
            x as u32,
            y as u32,
            cx as u32,
            cy as u32,
            flags,
        ]
        .into_iter()
        .enumerate()
        {
            let _ = env.cpu.write_u32(parameter + index as u32 * 4, value);
        }
        let Some(rect) = rectangle(x, y, cx, cy) else {
            temp_free(env, parameter);
            set_last_error(env, 87);
            return then(env, None);
        };
        then(
            env,
            Some(Prepared {
                change: Change {
                    hwnd: change.hwnd,
                    after,
                    rect,
                    flags,
                },
                parameter,
            }),
        )
    };
    if change.flags & 0x400 != 0 {
        changing(env, 0)
    } else {
        send(
            env,
            change.hwnd,
            wm::WINDOWPOSCHANGING,
            0,
            parameter,
            changing,
        )
    }
}
fn apply(env: &mut Env<'_>, prepared: &Prepared) -> bool {
    let change = prepared.change;
    let u = &mut env.ext.get_mut::<crate::Win32>().user;
    if !u.windows.contains_key(&env.sys.objects, &change.hwnd)
        || change.flags & SWP_NOZORDER == 0
            && !z_order::valid(u, &env.sys.objects, change.hwnd, change.after)
    {
        set_last_error(env, 1400);
        return false;
    }
    let window = u
        .windows
        .get_mut(&mut env.sys.objects, &change.hwnd)
        .unwrap();
    window.rect = change.rect;
    super::super::placement::position_changed(window);
    if change.flags & SWP_NOSIZE == 0 {
        window.invalidate_client(true);
    }
    if change.flags & SWP_NOZORDER == 0 {
        z_order::place(u, &mut env.sys.objects, change.hwnd, change.after);
    }
    true
}
fn notify(
    env: &mut Env<'_>,
    prepared: Prepared,
    then: impl FnOnce(&mut Env<'_>) -> Outcome + 'static,
) -> Outcome {
    let Change { hwnd, flags, .. } = prepared.change;
    send(
        env,
        hwnd,
        wm::WINDOWPOSCHANGED,
        0,
        prepared.parameter,
        move |env, _| {
            prepared.release(env);
            let show = if flags & SWP_SHOWWINDOW != 0 {
                Some(if flags & SWP_NOACTIVATE != 0 {
                    SW_SHOWNA
                } else {
                    SW_SHOW
                })
            } else if flags & SWP_HIDEWINDOW != 0 {
                Some(SW_HIDE)
            } else {
                None
            };
            if let Some(command) = show {
                show_window(env, hwnd, command, move |env, _| then(env))
            } else if flags & SWP_NOACTIVATE == 0 {
                let u = user(env);
                let mut root = hwnd;
                while let Some(parent) = u
                    .windows
                    .get(&env.sys.objects, &root)
                    .map(|w| w.parent)
                    .filter(|&p| p != 0)
                {
                    root = parent;
                }
                let activate = u.windows.get(&env.sys.objects, &root).is_some_and(|w| {
                    w.visible()
                        && w.style & WS_DISABLED == 0
                        && u.queues.get(&w.thread).is_none_or(|q| q.active != root)
                });
                if activate {
                    activate_window(env, root, then)
                } else {
                    then(env)
                }
            } else {
                then(env)
            }
        },
    )
}
pub(super) fn single(
    env: &mut Env<'_>,
    hwnd: u32,
    after: u32,
    rect: Rect,
    flags: u32,
    then: impl FnOnce(&mut Env<'_>, bool) -> Outcome + 'static,
) -> Outcome {
    prepare(
        env,
        Change {
            hwnd,
            after,
            rect,
            flags,
        },
        move |env, prepared| {
            let Some(prepared) = prepared else {
                return then(env, false);
            };
            if !apply(env, &prepared) {
                prepared.release(env);
                return then(env, false);
            }
            notify(env, prepared, move |env| then(env, true))
        },
    )
}
struct Batch {
    parent: Option<u32>,
    changes: Vec<Change>,
}
fn namespace(env: &mut Env<'_>) -> Option<Namespace> {
    if let Some(namespace) = user(env).deferred_positions {
        return Some(namespace);
    }
    let namespace = env.sys.objects.new_namespace()?;
    env.ext.get_mut::<crate::Win32>().user.deferred_positions = Some(namespace);
    Some(namespace)
}
fn batch_id(env: &Env<'_>, handle: u32) -> Option<Identity<Batch>> {
    env.sys
        .objects
        .lookup_alias::<Batch>(user(env).deferred_positions?, handle)
}
fn discard(env: &mut Env<'_>, handle: u32) {
    if let Some(ns) = user(env).deferred_positions {
        env.sys.objects.close_alias(ns, handle);
    }
}
#[adv_abi::win32(user32,name="BeginDeferWindowPos",level=implement)]
fn begin(env: &mut Env<'_>, count: i32) -> adv_abi::Handle {
    let error = if count < 0 { 87 } else { 8 };
    if count < 0 || count as u64 * 28 + 32 > u32::MAX as u64 {
        set_last_error(env, error);
        return adv_abi::Handle(0);
    }
    let mut changes = Vec::new();
    if changes.try_reserve_exact(count.max(1) as usize).is_err() {
        set_last_error(env, 8);
        return adv_abi::Handle(0);
    }
    let handle = namespace(env).and_then(|ns| {
        env.sys.objects.insert_host_in(
            ns,
            Batch {
                parent: None,
                changes,
            },
        )
    });
    if handle.is_none() {
        set_last_error(env, 8);
    }
    adv_abi::Handle(handle.unwrap_or(0))
}
#[adv_abi::win32(user32,name="DeferWindowPos",level=implement)]
#[allow(clippy::too_many_arguments)]
fn defer(
    env: &mut Env<'_>,
    batch: adv_abi::Handle,
    hwnd: adv_abi::Handle,
    after: adv_abi::Handle,
    x: i32,
    y: i32,
    cx: i32,
    cy: i32,
    flags: u32,
) -> adv_abi::Handle {
    let Some(id) = batch_id(env, batch.0) else {
        set_last_error(env, 1405);
        return adv_abi::Handle(0);
    };
    let u = user(env);
    let Some(window) = u.windows.get(&env.sys.objects, &hwnd.0) else {
        discard(env, batch.0);
        set_last_error(env, 1400);
        return adv_abi::Handle(0);
    };
    let parent = window.parent;
    if flags & SWP_NOZORDER == 0 && !z_order::valid(u, &env.sys.objects, hwnd.0, after.0) {
        discard(env, batch.0);
        set_last_error(env, 1400);
        return adv_abi::Handle(0);
    }
    let rect = match requested(env, hwnd.0, x, y, cx, cy, flags) {
        Ok(rect) => rect,
        Err(error) => {
            discard(env, batch.0);
            set_last_error(env, error);
            return adv_abi::Handle(0);
        }
    };
    let current = env.sys.objects.owned_mut(id).unwrap();
    if current.parent.is_some_and(|p| p != parent) {
        discard(env, batch.0);
        set_last_error(env, 87);
        return adv_abi::Handle(0);
    }
    current.parent = Some(parent);
    if let Some(existing) = current.changes.iter_mut().find(|c| c.hwnd == hwnd.0) {
        let width = if flags & SWP_NOSIZE == 0 {
            cx
        } else {
            existing.rect.width()
        };
        let height = if flags & SWP_NOSIZE == 0 {
            cy
        } else {
            existing.rect.height()
        };
        let (x, y) = if flags & SWP_NOMOVE == 0 {
            (x, y)
        } else {
            (existing.rect.left, existing.rect.top)
        };
        let Some(rect) = rectangle(x, y, width, height) else {
            discard(env, batch.0);
            set_last_error(env, 87);
            return adv_abi::Handle(0);
        };
        existing.rect = rect;
        if flags & SWP_NOZORDER == 0 {
            existing.after = after.0;
        }
        let and_flags =
            SWP_NOSIZE | SWP_NOMOVE | SWP_NOZORDER | SWP_NOACTIVATE | 0x8 | 0x200 | 0x400;
        existing.flags =
            (existing.flags & flags & and_flags) | ((existing.flags | flags) & !and_flags);
        if flags & SWP_SHOWWINDOW != 0 {
            existing.flags &= !SWP_HIDEWINDOW;
        }
        if flags & SWP_HIDEWINDOW != 0 {
            existing.flags &= !SWP_SHOWWINDOW;
        }
    } else {
        if current.changes.try_reserve(1).is_err() {
            discard(env, batch.0);
            set_last_error(env, 8);
            return adv_abi::Handle(0);
        }
        current.changes.push(Change {
            hwnd: hwnd.0,
            after: after.0,
            rect,
            flags,
        });
    }
    batch
}
#[adv_abi::win32(user32,name="EndDeferWindowPos",level=implement)]
fn end(env: &mut Env<'_>, batch: adv_abi::Handle) -> Outcome {
    let Some(id) = batch_id(env, batch.0) else {
        set_last_error(env, 1405);
        return Outcome::Return(0);
    };
    let changes = std::mem::take(&mut env.sys.objects.owned_mut(id).unwrap().changes);
    discard(env, batch.0);
    prepare_batch(env, changes.into(), Vec::new())
}
fn prepare_batch(
    env: &mut Env<'_>,
    mut pending: VecDeque<Change>,
    mut ready: Vec<Prepared>,
) -> Outcome {
    let Some(change) = pending.pop_front() else {
        for prepared in &ready {
            let change = prepared.change;
            let u = user(env);
            if !u.windows.contains_key(&env.sys.objects, &change.hwnd)
                || change.flags & SWP_NOZORDER == 0
                    && !z_order::valid(u, &env.sys.objects, change.hwnd, change.after)
            {
                for prepared in ready {
                    prepared.release(env);
                }
                set_last_error(env, 1400);
                return Outcome::Return(0);
            }
        }
        for prepared in &ready {
            if !apply(env, prepared) {
                for prepared in ready {
                    prepared.release(env);
                }
                return Outcome::Return(0);
            }
        }
        return notify_batch(env, ready.into());
    };
    prepare(env, change, move |env, prepared| {
        if let Some(prepared) = prepared {
            ready.push(prepared);
            prepare_batch(env, pending, ready)
        } else {
            for prepared in ready {
                prepared.release(env);
            }
            Outcome::Return(0)
        }
    })
}
fn notify_batch(env: &mut Env<'_>, mut ready: VecDeque<Prepared>) -> Outcome {
    if let Some(prepared) = ready.pop_front() {
        notify(env, prepared, move |env| notify_batch(env, ready))
    } else {
        Outcome::Return(1)
    }
}
pub(super) fn register(registry: &mut adv_abi::Registry) {
    for api in [begin_ABI, defer_ABI, end_ABI] {
        registry.register(api);
    }
}
