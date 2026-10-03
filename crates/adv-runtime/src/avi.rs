//! AVIFile library lifetime. File/stream decoding is registered only as its
//! persona is implemented; initialization does not claim those capabilities.
use pal_machine::{Conv, Env, Outcome, Registry};

#[derive(Default)]
struct Library {
    references: u32,
}
impl Library {
    fn initialize(&mut self) {
        self.references = self.references.saturating_add(1);
    }
    fn release(&mut self) {
        self.references = self.references.saturating_sub(1);
    }
}

pub fn register(registry: &mut Registry) {
    registry.add(
        "avifil32.dll",
        "AVIFileInit",
        Conv::Stdcall { argc: 0 },
        initialize,
    );
    registry.add(
        "avifil32.dll",
        "AVIFileExit",
        Conv::Stdcall { argc: 0 },
        release,
    );
}
fn initialize(env: &mut Env<'_>) -> Outcome {
    if env.ext.try_get::<Library>().is_none() {
        env.ext.insert(Library::default());
    }
    env.ext.get_mut::<Library>().initialize();
    // The native function is void. This EAX is not an AVI success HRESULT.
    Outcome::Return(0)
}
fn release(env: &mut Env<'_>) -> Outcome {
    if let Some(library) = env.ext.try_get_mut::<Library>() {
        library.release();
    }
    Outcome::Return(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn nested_initialization_keeps_the_library_until_the_last_exit() {
        let mut library = Library::default();
        library.initialize();
        library.initialize();
        assert_eq!(library.references, 2);
        library.release();
        assert_eq!(library.references, 1);
        library.release();
        assert_eq!(library.references, 0);
        library.initialize();
        assert_eq!(library.references, 1);
    }
    #[test]
    fn initialization_belongs_to_each_guest_session() {
        let mut first = Library::default();
        let second = Library::default();
        first.initialize();
        assert_eq!(first.references, 1);
        assert_eq!(second.references, 0);
    }
}
