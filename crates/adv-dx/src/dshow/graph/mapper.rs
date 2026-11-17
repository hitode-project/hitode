//! The graph's filter mapper interfaces. Like quartz, the graph aggregates the
//! system FilterMapper2, which Wine's quartz provides, creating it on first use.
use super::*;
use crate::dshow::host::create_wine;

const IID_IFILTERMAPPER: Iid = quartz(0x56a8_68a3);
pub(super) const IID_IFILTERMAPPER2: Iid = guid(
    0xb79b_b0b0,
    0x33c1,
    0x11d1,
    [0xab, 0xe1, 0x00, 0xa0, 0xc9, 0x05, 0xf3, 0x75],
);
const IID_IFILTERMAPPER3: Iid = guid(
    0xb79b_b0b1,
    0x33c1,
    0x11d1,
    [0xab, 0xe1, 0x00, 0xa0, 0xc9, 0x05, 0xf3, 0x75],
);
const CLSID_FILTERMAPPER2: Iid = guid(
    0xcda4_2200,
    0xbd88,
    0x11d0,
    [0xbd, 0x4e, 0x00, 0xa0, 0xc9, 0x11, 0xce, 0x86],
);
/// The graph whose mapper answers `iid` through `this`, if `iid` names a mapper.
pub(crate) fn target(env: &Env<'_>, this: u32, iid: u32) -> Option<u32> {
    let requested = read_guid(env, iid).ok()?;
    [IID_IFILTERMAPPER, IID_IFILTERMAPPER2, IID_IFILTERMAPPER3]
        .contains(&requested)
        .then(|| graph_of(env, this))?
}

/// QueryInterface through the aggregated mapper. Its own references count
/// against the graph's controlling IUnknown.
pub(crate) fn query(env: &mut Env<'_>, graph: u32, iid: u32, output: u32) -> Outcome {
    if output == 0 {
        return Outcome::Return(E_POINTER);
    }
    let _ = env.cpu.write_u32(output, 0);
    let mapper = g(env, graph).mapper;
    if mapper != 0 {
        return guest_call(env, mapper, 0, vec![iid, output], |_, hr| {
            Outcome::Return(hr)
        });
    }
    let controlling = g(env, graph).controlling();
    let unknown = adv_os::com::IUNKNOWN;
    create_wine(
        env,
        CLSID_FILTERMAPPER2,
        controlling,
        unknown,
        move |env, mapper, _| {
            if mapper == 0 {
                return Outcome::Return(E_NOINTERFACE);
            }
            g(env, graph).mapper = mapper;
            guest_call(env, mapper, 0, vec![iid, output], |_, hr| {
                Outcome::Return(hr)
            })
        },
    )
}
