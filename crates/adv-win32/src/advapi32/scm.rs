//! Read-only SCM frontend over the virtual OS catalog and process-local aliases.
mod read_only;
pub(super) fn register(registry: &mut adv_abi::Registry) {
    read_only::register(registry);
}
