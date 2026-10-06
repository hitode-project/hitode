//! USER icon/cursor identities use the same process payload table as windows.
//! Resource decoding and the existing headless cursor policy are unchanged.
use adv_machine::{Identity, Namespace, Objects};
use std::collections::BTreeMap;

struct UserObject {
    description: String,
    stock: bool,
    _image: Option<IconImage>,
}

/// Extracted icon pixels remain owned by the same USER object DestroyIcon closes.
/// The AND mask is retained separately so monochrome XOR pixels are not lost.
pub(crate) struct IconImage {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u8>,
    pub mask: Vec<u8>,
}

#[derive(Default)]
pub struct UserObjects {
    namespace: Option<Namespace>,
    /// Non-owning traversal index, never a second lifetime or payload store.
    index: BTreeMap<u32, Identity<UserObject>>,
}

impl UserObjects {
    pub(crate) fn contains(&self, objects: &Objects, handle: u32) -> bool {
        self.index
            .get(&handle)
            .is_some_and(|&id| objects.owned(id).is_some())
    }

    fn namespace(&mut self, objects: &mut Objects) -> Option<Namespace> {
        if self.namespace.is_none() {
            self.namespace = objects.new_namespace_in(0x0007_0000, 0x00ff_fffc);
        }
        self.namespace
    }

    pub(super) fn find(&self, objects: &Objects, description: &str) -> Option<u32> {
        self.index.iter().find_map(|(&handle, &identity)| {
            (objects.owned(identity)?.description == description).then_some(handle)
        })
    }

    pub(super) fn create(&mut self, objects: &mut Objects, description: String) -> Option<u32> {
        self.create_metadata(objects, description, false)
    }

    pub(super) fn create_metadata(
        &mut self,
        objects: &mut Objects,
        description: String,
        shared: bool,
    ) -> Option<u32> {
        let namespace = self.namespace(objects)?;
        let handle = objects.insert_host_in(
            namespace,
            UserObject {
                description,
                stock: shared,
                _image: None,
            },
        )?;
        self.index
            .insert(handle, objects.lookup_alias(namespace, handle)?);
        Some(handle)
    }

    pub(crate) fn create_icon(
        &mut self,
        objects: &mut Objects,
        description: String,
        image: IconImage,
    ) -> Option<u32> {
        self.create_image(objects, description, image, false)
    }

    pub(super) fn create_image(
        &mut self,
        objects: &mut Objects,
        description: String,
        image: IconImage,
        shared: bool,
    ) -> Option<u32> {
        let pixels = image.width.checked_mul(image.height)? as usize;
        if pixels == 0 || image.pixels.len() != pixels.checked_mul(4)? || image.mask.len() != pixels
        {
            return None;
        }
        let namespace = self.namespace(objects)?;
        let handle = objects.insert_host_in(
            namespace,
            UserObject {
                description,
                stock: shared,
                _image: Some(image),
            },
        )?;
        self.index
            .insert(handle, objects.lookup_alias(namespace, handle)?);
        Some(handle)
    }

    pub(crate) fn destroy_icon(&mut self, objects: &mut Objects, handle: u32) {
        self.remove(objects, handle);
    }

    pub(crate) fn image<'a>(&self, objects: &'a Objects, handle: u32) -> Option<&'a IconImage> {
        objects.owned(*self.index.get(&handle)?)?._image.as_ref()
    }

    pub(super) fn stock(&mut self, objects: &mut Objects, handle: u32) -> Option<u32> {
        if self
            .index
            .get(&handle)
            .is_some_and(|&id| objects.owned(id).is_some())
        {
            return Some(handle);
        }
        let namespace = self.namespace(objects)?;
        if !objects.insert_host_at(
            namespace,
            handle,
            UserObject {
                description: format!("system resource {handle:#x}"),
                stock: true,
                _image: None,
            },
        ) {
            return None;
        }
        self.index
            .insert(handle, objects.lookup_alias(namespace, handle)?);
        Some(handle)
    }

    pub(super) fn remove(&mut self, objects: &mut Objects, handle: u32) {
        let Some(&identity) = self.index.get(&handle) else {
            return;
        };
        // Stock resources belong to the process and are shared by callers.
        if objects.owned(identity).is_some_and(|value| value.stock) {
            return;
        }
        if let Some(namespace) = self.namespace {
            objects.close_alias(namespace, handle);
        }
        self.index.remove(&handle);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use adv_machine::ObjectLimits;

    #[test]
    fn stock_and_dynamic_resources_obey_common_capacity_without_becoming_kernel_handles() {
        let mut objects = Objects::with_limits(ObjectLimits {
            objects: 2,
            ..ObjectLimits::default()
        });
        let mut resources = UserObjects::default();
        let stock = resources.stock(&mut objects, 0x60010).unwrap();
        assert_eq!(resources.stock(&mut objects, stock), Some(stock));
        let first = resources
            .create(&mut objects, "icon module 1".into())
            .unwrap();
        assert_eq!(resources.find(&objects, "icon module 1"), Some(first));
        assert!(
            resources
                .create(&mut objects, "icon module 2".into())
                .is_none()
        );
        assert!(objects.close(stock).is_none());
        assert!(objects.close(first).is_none());
        resources.remove(&mut objects, stock);
        assert_eq!(objects.object_count(), 2);
        resources.remove(&mut objects, first);
        assert_eq!(resources.find(&objects, "icon module 1"), None);
        let replacement = resources
            .create(&mut objects, "icon module 1".into())
            .unwrap();
        assert_ne!(replacement, first);
        resources.remove(&mut objects, first);
        assert_eq!(resources.find(&objects, "icon module 1"), Some(replacement));
        assert_eq!(objects.object_count(), 2);
    }
}
