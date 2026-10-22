//! Version exports select shared handlers through the authoritative ABI table.
//! Export presence is independent of signatures: an SDK declaration does not
//! imply that every older runtime exports that name.
use adv_abi::{Api, ImplementationLevel, Registry, signature};
use adv_machine::Handler;
use std::{collections::HashSet, sync::OnceLock};

#[derive(Clone, Copy)]
pub(super) struct Profile {
    pub module: &'static str,
    pub invalid_parameters: bool,
    pub multibyte_utf8: bool,
}

pub(super) const PROFILES: &[Profile] = &[
    Profile {
        module: "msvcrt20.dll",
        invalid_parameters: false,
        multibyte_utf8: false,
    },
    Profile {
        module: "msvcrt40.dll",
        invalid_parameters: false,
        multibyte_utf8: false,
    },
    Profile {
        module: "msvcrt.dll",
        invalid_parameters: true,
        multibyte_utf8: false,
    },
    Profile {
        module: "msvcr70.dll",
        invalid_parameters: false,
        multibyte_utf8: false,
    },
    Profile {
        module: "msvcr71.dll",
        invalid_parameters: false,
        multibyte_utf8: false,
    },
    Profile {
        module: "msvcr80.dll",
        invalid_parameters: true,
        multibyte_utf8: false,
    },
    Profile {
        module: "msvcr90.dll",
        invalid_parameters: true,
        multibyte_utf8: false,
    },
    Profile {
        module: "msvcr100.dll",
        invalid_parameters: true,
        multibyte_utf8: false,
    },
    Profile {
        module: "msvcr110.dll",
        invalid_parameters: true,
        multibyte_utf8: false,
    },
    Profile {
        module: "msvcr120.dll",
        invalid_parameters: true,
        multibyte_utf8: false,
    },
    Profile {
        module: "ucrtbase.dll",
        invalid_parameters: true,
        multibyte_utf8: true,
    },
    Profile {
        module: "vcruntime140.dll",
        invalid_parameters: true,
        multibyte_utf8: false,
    },
];

fn catalog() -> &'static HashSet<(&'static str, &'static str, &'static str)> {
    static CATALOG: OnceLock<HashSet<(&str, &str, &str)>> = OnceLock::new();
    CATALOG.get_or_init(|| {
        include_str!("../../../../data/abi/crt-exports.tsv")
            .lines()
            .filter(|line| !line.starts_with('#'))
            .filter_map(|line| {
                let mut fields = line.split('\t');
                let (module, name, kind) = (fields.next()?, fields.next()?, fields.next()?);
                Some((module, name, kind))
            })
            .collect()
    })
}

pub(super) fn profile(module: &str) -> Profile {
    let module = super::crt_module(module);
    PROFILES
        .iter()
        .find(|p| p.module == module)
        .copied()
        .expect("CRT profile")
}

/// This table contains behavior, never argument counts. All conventions come
/// from data/abi, including variadic mandatory prefixes and API-set exports.
pub(super) fn register(registry: &mut Registry, exports: &[(&str, Handler)]) {
    for declaration in signature::signatures() {
        if !PROFILES.iter().any(|p| p.module == declaration.module)
            && !declaration.module.starts_with("api-ms-win-crt-")
        {
            continue;
        }
        if !catalog().contains(&(declaration.module, declaration.name, "code")) {
            continue;
        }
        if registry
            .level(declaration.module, declaration.name)
            .is_some()
        {
            continue;
        }
        if let Some((_, handler)) = exports.iter().find(|(name, _)| *name == declaration.name) {
            registry.register(Api {
                module: declaration.module,
                name: declaration.name,
                conv: declaration.conv.expect("CRT code declaration"),
                handler: *handler,
                env_leaf: None,
                level: ImplementationLevel::Implement,
            });
        }
    }
}

/// Storage declarations select the same version/catalog matrix as code.
pub(super) fn register_data(registry: &mut Registry, exports: &[(&str, &[u8])]) {
    for declaration in signature::signatures() {
        if declaration.data_size.is_none()
            || !PROFILES
                .iter()
                .any(|profile| profile.module == declaration.module)
            || !catalog().contains(&(declaration.module, declaration.name, "data"))
        {
            continue;
        }
        if let Some((_, bytes)) = exports.iter().find(|(name, _)| *name == declaration.name) {
            registry
                .register_data(declaration.module, declaration.name, bytes)
                .expect("CRT storage matches its generated declaration");
        }
    }
}
