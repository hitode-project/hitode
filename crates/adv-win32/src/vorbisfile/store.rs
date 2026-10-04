//! The guest OggVorbis_File address is an alias, never a kernel handle. A slot
//! stays in Objects while synchronous decoding temporarily moves out its state.
use adv_machine::{Identity, Namespace, Object, Objects};

use super::{Stats, Stream};

pub(super) struct StreamSlot {
    stream: Option<Stream>,
}

#[derive(Default)]
pub(super) struct Provider {
    namespace: Option<Namespace>,
    pub stats: Stats,
}

impl Provider {
    pub fn count(&self, objects: &Objects) -> usize {
        self.namespace.map_or(0, |ns| objects.alias_count(ns))
    }

    pub fn identity(&self, objects: &Objects, vf: u32) -> Option<Identity<StreamSlot>> {
        objects.lookup_alias(self.namespace?, vf)
    }

    pub fn insert(&mut self, objects: &mut Objects, vf: u32, stream: Stream) -> bool {
        if self.namespace.is_none() {
            self.namespace = objects.new_namespace();
        }
        self.namespace.is_some_and(|ns| {
            objects.insert_host_at(
                ns,
                vf,
                StreamSlot {
                    stream: Some(stream),
                },
            )
        })
    }

    pub fn get<'a>(&self, objects: &'a Objects, vf: u32) -> Option<&'a Stream> {
        Self::at(objects, self.identity(objects, vf)?)
    }

    pub fn get_mut<'a>(&self, objects: &'a mut Objects, vf: u32) -> Option<&'a mut Stream> {
        let identity = self.identity(objects, vf)?;
        Self::at_mut(objects, identity)
    }

    pub fn at(objects: &Objects, identity: Identity<StreamSlot>) -> Option<&Stream> {
        objects.owned(identity)?.stream.as_ref()
    }

    pub fn at_mut(objects: &mut Objects, identity: Identity<StreamSlot>) -> Option<&mut Stream> {
        objects.owned_mut(identity)?.stream.as_mut()
    }

    pub fn take(&self, objects: &mut Objects, vf: u32) -> Option<(Identity<StreamSlot>, Stream)> {
        let identity = self.identity(objects, vf)?;
        Some((identity, objects.owned_mut(identity)?.stream.take()?))
    }

    pub fn put(objects: &mut Objects, identity: Identity<StreamSlot>, stream: Stream) {
        let slot = objects
            .owned_mut(identity)
            .expect("synchronous decoder keeps its Objects identity live");
        assert!(slot.stream.is_none(), "decoder restored exactly once");
        slot.stream = Some(stream);
    }

    pub fn remove(&self, objects: &mut Objects, vf: u32) -> Option<Stream> {
        // An empty slot is exclusively borrowed by synchronous host work. It
        // must not be removed, nor have its resource budget lent to another open.
        self.get(objects, vf)?;
        let object = objects.close_alias(self.namespace?, vf)??;
        let Object::Host(value) = object else {
            unreachable!("typed codec alias must name a host slot")
        };
        value.downcast::<StreamSlot>().ok()?.stream
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use adv_device::vorbis::Stream as CodecStream;
    use adv_machine::ObjectLimits;

    fn fixture_stream() -> Stream {
        Stream {
            datasource: 1,
            callbacks: [0; 4],
            arena: 0,
            seekable: false,
            partial: false,
            busy: false,
            codec: CodecStream::new(Vec::new()),
            comment_memory: Vec::new(),
        }
    }

    #[test]
    fn a_temporarily_moved_decoder_keeps_identity_alias_and_resource_budget() {
        let mut objects = Objects::with_limits(ObjectLimits {
            objects: 1,
            aliases: 1,
            ..ObjectLimits::default()
        });
        let mut provider = Provider::default();
        assert!(provider.insert(&mut objects, 0x410003, fixture_stream()));
        let identity = provider.identity(&objects, 0x410003).unwrap();
        let (borrowed, stream) = provider.take(&mut objects, 0x410003).unwrap();
        assert_eq!(borrowed, identity);
        assert_eq!(objects.object_count(), 1);
        assert_eq!(provider.count(&objects), 1);
        assert_eq!(objects.strong_count(identity), Some(1));
        assert!(provider.get(&objects, 0x410003).is_none());
        assert!(provider.remove(&mut objects, 0x410003).is_none());
        assert!(!provider.insert(&mut objects, 0x420000, fixture_stream()));
        assert!(!provider.insert(&mut objects, 0x410003, fixture_stream()));
        Provider::put(&mut objects, borrowed, stream);
        assert_eq!(provider.identity(&objects, 0x410003), Some(identity));
        assert!(provider.get(&objects, 0x410003).is_some());
        provider.remove(&mut objects, 0x410003).unwrap();
        assert_eq!(provider.count(&objects), 0);
        assert_eq!(objects.object_count(), 0);
    }

    #[test]
    fn reopening_the_same_guest_structure_does_not_revive_its_old_identity() {
        let mut objects = Objects::default();
        let mut provider = Provider::default();
        let kernel = objects.insert(Object::Event {
            manual_reset: false,
            signaled: false,
        });
        assert!(provider.insert(&mut objects, kernel, fixture_stream()));
        let old = provider.identity(&objects, kernel).unwrap();
        assert!(objects.host::<StreamSlot>(kernel).is_none());
        // A numerically equal kernel handle closes only the event namespace.
        assert!(objects.close(kernel).is_some());
        assert!(Provider::at(&objects, old).is_some());
        provider.remove(&mut objects, kernel).unwrap();
        assert!(provider.insert(&mut objects, kernel, fixture_stream()));
        let new = provider.identity(&objects, kernel).unwrap();
        assert_ne!(old, new);
        assert!(Provider::at(&objects, old).is_none());
        assert!(Provider::at_mut(&mut objects, old).is_none());
        assert!(Provider::at(&objects, new).is_some());
    }
}
