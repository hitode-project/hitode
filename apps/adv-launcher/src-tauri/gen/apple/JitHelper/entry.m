// Register the Rust request handler before Foundation starts the extension.
extern void adv_jit_helper_init(void);

__attribute__((constructor))
static void register_jit_handler(void) {
    adv_jit_helper_init();
}
